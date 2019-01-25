// vim: tw=80
use super::*;
use quote::{ToTokens, quote};
use std::borrow::Borrow;
use syn::{
    braced,
    parse::{Parse, ParseStream},
    spanned::Spanned,
    Token
};

struct MethodTypes {
    is_static: bool,
    input_type: syn::TypeTuple,
    output_type: syn::Type,
    /// Type of Expectation object stored in the mock structure
    expect_obj: syn::Type,
    /// Type of Expectation returned by the expect method
    expectation: syn::Type,
    /// Method to call when invoking the expectation
    call: syn::Ident,
    /// Any turbofish needed when invoking the expectation
    call_turbofish: Option<syn::MethodTurbofish>
}

pub(crate) struct Mock {
    pub(crate) vis: syn::Visibility,
    pub(crate) name: syn::Ident,
    pub(crate) generics: syn::Generics,
    pub(crate) methods: Vec<syn::TraitItemMethod>,
    pub(crate) traits: Vec<syn::ItemTrait>
}

impl Mock {
    pub(crate) fn gen(&self) -> TokenStream {
        let mut output = TokenStream::new();
        let mut mock_body = TokenStream::new();
        let mock_struct_name = gen_mock_ident(&self.name);
        let subs = self.traits.iter().map(|trait_| {
            (trait_.ident.to_string(), trait_.generics.clone())
        }).collect::<Vec<_>>();
        // generate the mock structure
        gen_struct(&self.vis, &self.name, &self.generics, &subs, &self.methods)
            .to_tokens(&mut output);
        // generate sub structures
        for trait_ in self.traits.iter() {
            let sub_mock = syn::Ident::new(
                &format!("{}_{}", &self.name, &trait_.ident),
                Span::call_site());
            let methods = trait_.items.iter().filter_map(|item| {
                if let syn::TraitItem::Method(m) = item {
                    Some(m)
                } else {
                    None
                }
            }).collect::<Vec<_>>();
            gen_struct(&syn::Visibility::Inherited, &sub_mock,
                       &trait_.generics, &[], &methods)
                .to_tokens(&mut output);
        }
        // generate methods on the mock structure itself
        for meth in self.methods.iter() {
            // All mocked methods are public
            let pub_token = syn::token::Pub{span: Span::call_site()};
            let vis = syn::Visibility::Public(syn::VisPublic{pub_token});
            let (mm, em) = gen_mock_method(&mock_struct_name, None, &vis,
                                           &meth.sig, None);
            mm.to_tokens(&mut mock_body);
            em.to_tokens(&mut mock_body);
        }
        // generate methods on traits
        let generics = &self.generics;
        let (ig, tg, wc) = generics.split_for_impl();
        quote!(impl #ig #mock_struct_name #tg #wc {#mock_body})
            .to_tokens(&mut output);
        for trait_ in self.traits.iter() {
            mock_trait_methods(&mock_struct_name, Some(&generics), &trait_)
                .to_tokens(&mut output);
        }
        output
    }
}

impl Parse for Mock {
    fn parse(input: ParseStream) -> syn::parse::Result<Self> {
        let vis: syn::Visibility = input.parse()?;
        let name: syn::Ident = input.parse()?;
        let generics: syn::Generics = input.parse()?;

        let impl_content;
        let _brace_token = braced!(impl_content in input);
        let methods_item: syn::punctuated::Punctuated<syn::TraitItem, Token![;]>
            = impl_content.parse_terminated(syn::TraitItem::parse)?;
        let mut methods = Vec::new();
        for method in methods_item.iter() {
            match method {
                syn::TraitItem::Method(meth) => methods.push(meth.clone()),
                _ => {
                    return Err(input.error("Unsupported in this context"));
                }
            }
        }

        let mut traits = Vec::new();
        while !input.is_empty() {
            let trait_: syn::ItemTrait = input.parse()?;
            traits.push(trait_);
        }

        Ok(Mock{vis, name, generics, methods, traits})
    }
}

/// Generate a mock method and its expectation method
fn gen_mock_method(mock_ident: &syn::Ident,
                   defaultness: Option<&syn::token::Default>,
                   vis: &syn::Visibility,
                   sig: &syn::MethodSig,
                   sub: Option<&syn::Ident>) -> (TokenStream, TokenStream)
{
    assert!(sig.decl.variadic.is_none(),
        "MockAll does not yet support variadic functions");
    let mut mock_output = TokenStream::new();
    let mut expect_output = TokenStream::new();
    let constness = sig.constness;
    let unsafety = sig.unsafety;
    let asyncness = sig.asyncness;
    let abi = &sig.abi;
    let fn_token = &sig.decl.fn_token;
    let ident = &sig.ident;
    let generics = &sig.decl.generics;
    let inputs = &sig.decl.inputs;
    let output = &sig.decl.output;

    // First the mock method
    quote!(#defaultness #vis #constness #unsafety #asyncness #abi
           #fn_token #ident #generics (#inputs) #output)
        .to_tokens(&mut mock_output);

    let sub_name = if let Some(s) = sub {
        format!("{}_", s)
    } else {
        "".to_string()
    };
    let meth_types = method_types(sig);
    let input_type = &meth_types.input_type;
    let output_type = &meth_types.output_type;
    let expectation = &meth_types.expectation;
    let call = &meth_types.call;
    let call_turbofish = &meth_types.call_turbofish;
    let mut args = Vec::new();
    let expect_obj_name = if meth_types.is_static {
        let name = syn::Ident::new(
            &format!("{}_{}{}_expectation", mock_ident, sub_name, sig.ident),
            Span::call_site());
        quote!(#name)
    } else if let Some(s) = sub {
        let sub_struct = syn::Ident::new(&format!("{}_expectations", s),
            Span::call_site());
        quote!(self.#sub_struct.#ident)
    } else {
        quote!(self.#ident)
    };
    for p in sig.decl.inputs.iter() {
        match p {
            syn::FnArg::SelfRef(_) | syn::FnArg::SelfValue(_) => {
                // Don't output the "self" argument
            },
            syn::FnArg::Captured(arg) => {
                let pat = &arg.pat;
                args.push(quote!(#pat));
            },
            _ => compile_error(p.span(),
                "Should be unreachable for normal Rust code")
        }
    }

    if meth_types.is_static {
        quote!({
            #expect_obj_name.lock().unwrap().call((#(#args),*))
        })
    } else {
        quote!({
            #expect_obj_name.#call#call_turbofish((#(#args),*))
        })
    }.to_tokens(&mut mock_output);

    // Then the expectation method
    let expect_ident = syn::Ident::new(&format!("expect_{}", sig.ident),
                                       sig.ident.span());
    if meth_types.is_static {
        let name = syn::Ident::new(
            &format!("{}_{}{}_expectation", mock_ident, sub_name, sig.ident),
            Span::call_site());
        let mut g = generics.clone();
        let lt = syn::Lifetime::new("'guard", Span::call_site());
        let ltd = syn::LifetimeDef::new(lt);
        g.params.push(syn::GenericParam::Lifetime(ltd.clone()));
        quote!(pub fn #expect_ident #g()
               -> ::mockall::ExpectationGuard<#ltd, #input_type, #output_type>
            {
                ::mockall::ExpectationGuard::new(
                    #name.lock().unwrap()
                )
            }
        )
    } else {
        quote!(pub fn #expect_ident #generics(&mut self)
               -> &mut #expectation<#input_type, #output_type> {
            #expect_obj_name.expect#call_turbofish()
        })
    }.to_tokens(&mut expect_output);

    (mock_output, expect_output)
}

fn gen_struct<T>(vis: &syn::Visibility,
                 ident: &syn::Ident,
                 generics: &syn::Generics,
                 subs: &[(String, syn::Generics)],
                 methods: &[T]) -> TokenStream
    where T: Borrow<syn::TraitItemMethod>
{
    let mut output = TokenStream::new();
    let ident = gen_mock_ident(&ident);
    let mut body = TokenStream::new();
    let mut statics = TokenStream::new();

    // Make Expectation fields for each method
    for (sub, sub_generics) in subs.iter() {
        let spn = Span::call_site();
        let (_, tg, _) = sub_generics.split_for_impl();
        let sub_struct = syn::Ident::new(&format!("{}_expectations", sub), spn);
        let sub_mock = syn::Ident::new(&format!("{}_{}", ident, sub), spn);
        quote!(#sub_struct: #sub_mock #tg,).to_tokens(&mut body);
    }
    for meth in methods.iter() {
        let method_ident = &meth.borrow().sig.ident;
        let meth_types = method_types(&meth.borrow().sig);
        let expect_obj = &meth_types.expect_obj;
        if meth_types.is_static {
            let name = syn::Ident::new(
                &format!("{}_{}_expectation", ident, method_ident),
                Span::call_site());
            quote!(static ref #name: ::std::sync::Mutex<#expect_obj> =
                   ::std::sync::Mutex::new(::mockall::Expectations::new());
                ).to_tokens(&mut statics);
        } else {
            quote!(#method_ident: #expect_obj,).to_tokens(&mut body);
        }
    }

    // Make PhantomData fields, if necessary
    let mut count = 0;
    for param in generics.params.iter() {
        let phname = format!("_t{}", count);
        let phident = syn::Ident::new(&phname, Span::call_site());
        match param {
            syn::GenericParam::Lifetime(l) => {
                if !l.bounds.is_empty() {
                    compile_error(l.bounds.span(),
                        "#automock does not yet support lifetime bounds on structs");
                }
                let lifetime = &l.lifetime;
                quote!(#phident: ::std::marker::PhantomData<&#lifetime ()>,)
                    .to_tokens(&mut body);
            },
            syn::GenericParam::Type(tp) => {
                let ty = &tp.ident;
                quote!(#phident: ::std::marker::PhantomData<#ty>,)
                    .to_tokens(&mut body);
            },
            syn::GenericParam::Const(_) => {
                compile_error(param.span(),
                    "#automock does not yet support generic constants");
            }
        }
        count += 1;
    }
    quote!(
        #[derive(Default)]
        #vis struct #ident #generics {
            #body
        }
    ).to_tokens(&mut output);
    if ! statics.is_empty() {
        quote!(::mockall::lazy_static! {
            #statics
        }).to_tokens(&mut output);
    }

    output
}

fn method_types(sig: &syn::MethodSig) -> MethodTypes {
    let mut is_static = true;
    let mut elems
        = syn::punctuated::Punctuated::<syn::Type, Token![,]>::new();
    let is_generic = !sig.decl.generics.params.is_empty();
    for fn_arg in sig.decl.inputs.iter() {
        match fn_arg {
            syn::FnArg::Captured(arg) => elems.push(arg.ty.clone()),
            syn::FnArg::SelfRef(_) => {
                is_static = false;
            },
            syn::FnArg::SelfValue(_) => {
                is_static = false;
            },
            _ => compile_error(fn_arg.span(),
                "Should be unreachable for normal Rust code")
        }
    }
    let paren_token = syn::token::Paren::default();
    let input_type = syn::TypeTuple{paren_token, elems};

    let span = Span::call_site();
    let (output_type, partial_ex, call) = match &sig.decl.output {
        syn::ReturnType::Default => {
            let paren_token = syn::token::Paren{span};
            let elems = syn::punctuated::Punctuated::new();
            (
                syn::Type::Tuple(syn::TypeTuple{paren_token, elems}),
                syn::Ident::new("Expectation", span),
                syn::Ident::new("call", span)
            )
        },
        syn::ReturnType::Type(_, ty) => {
            match ty.as_ref() {
                syn::Type::Reference(r) => {
                    if let Some(ref lt) = r.lifetime {
                        if lt.ident != &"static" {
                            compile_error(r.span(), "Non-'static non-'self lifetimes are not yet supported");
                        }
                    }
                    if r.mutability.is_some() {
                        (
                            (*r.elem).clone(),
                            syn::Ident::new("RefMutExpectation", span),
                            syn::Ident::new("call_mut", span)
                        )
                    } else {
                        (
                            (*r.elem).clone(),
                            syn::Ident::new("RefExpectation", span),
                            syn::Ident::new("call", span)
                        )
                    }
                },
                _ => (
                    (**ty).clone(),
                    syn::Ident::new("Expectation", span),
                    syn::Ident::new("call", span)
                )
            }
        }
    };

    let call_turbofish = if is_generic {
        let mut args = syn::punctuated::Punctuated::new();
        let input_type_type = syn::Type::Tuple(input_type.clone());
        args.push(syn::GenericMethodArgument::Type(input_type_type));
        args.push(syn::GenericMethodArgument::Type(output_type.clone()));
        Some(syn::MethodTurbofish{
            colon2_token: syn::token::Colon2::default(),
            lt_token: syn::token::Lt::default(),
            args,
            gt_token: syn::token::Gt::default()
        })
    } else {
        None
    };
    let expect_obj = if is_generic {
        let name = syn::Ident::new(&format!("Generic{}s", partial_ex), span);
        syn::parse2(quote!(::mockall::#name)).unwrap()
    } else {
        let name = syn::Ident::new(&format!("{}s", partial_ex), span);
        syn::parse2(
            quote!(::mockall::#name<#input_type, #output_type>)
        ).unwrap()
    };
    let expect_ts = quote!(::mockall::#partial_ex);
    let expectation: syn::Type = syn::parse2(expect_ts).unwrap();

    MethodTypes{is_static, input_type, output_type, expectation, call,
                expect_obj, call_turbofish}
}

/// Generate mock methods for a Trait
///
/// # Parameters
///
/// * `mock_ident`:         Name of the mock structure to generate
/// * `struct_generics`:    If provided, use these generic fields for the
///                         Mock struct.  Otherwise, generate the struct's
///                         generics from the Trait
/// * `item`:               The trait whose methods are being mocked
fn mock_trait_methods(mock_ident: &syn::Ident,
                      struct_generics: Option<&syn::Generics>,
                      item: &syn::ItemTrait) -> TokenStream
{
    let mut output = TokenStream::new();
    let mut mock_body = TokenStream::new();
    let mut expect_body = TokenStream::new();

    for trait_item in item.items.iter() {
        match trait_item {
            syn::TraitItem::Const(_) => {
                // Nothing to implement
            },
            syn::TraitItem::Method(meth) => {
                let (mock_meth, expect_meth) = gen_mock_method(
                    mock_ident,
                    None,
                    &syn::Visibility::Inherited,
                    &meth.sig,
                    Some(&item.ident)
                );
                mock_meth.to_tokens(&mut mock_body);
                expect_meth.to_tokens(&mut expect_body);
            },
            syn::TraitItem::Type(ty) => {
                if !ty.generics.params.is_empty() {
                    compile_error(ty.generics.span(),
                        "Mockall does not yet support generic associated types");
                }
                if ty.default.is_some() {
                    // Trait normally can't get here (unless the
                    // associated_type_defaults feature is enabled), but we can
                    // get here from mock! if invoked like
                    // mock!
                    // mock!{
                    //     Foo { }
                    //     trait Bar {
                    //         type A=B;
                    //     }
                    // }
                    ty.to_tokens(&mut mock_body)
                } else {
                    compile_error(ty.span(), "Associated types must be made concrete for mocking.");
                }
            },
            _ => {
                compile_error(trait_item.span(),
                    "This impl item is not yet supported by MockAll");
            }
        }
    }

    // Put all mock methods in one impl block
    item.unsafety.to_tokens(&mut output);
    let ident = &item.ident;
    let trait_generics = &item.generics;
    let (merged_g, reduced_struct_g) = match struct_generics {
        None => (trait_generics.clone(), trait_generics.split_for_impl().1),
        Some(g) => {
            (g.clone(), g.split_for_impl().1)
        }
    };
    let reduced_trait_g = trait_generics.split_for_impl().1;
    quote!(impl #merged_g #ident #reduced_trait_g
           for #mock_ident #reduced_struct_g {
        #mock_body
    }).to_tokens(&mut output);

    // Put all expect methods in a separate impl block.  This is necessary when
    // mocking a trait impl, where we can't add any new methods
    quote!(impl #merged_g #mock_ident #reduced_struct_g {
        #expect_body
    }).to_tokens(&mut output);

    output
}

pub(crate) fn do_mock(input: TokenStream) -> TokenStream {
    let mock: Mock = match syn::parse2(input) {
        Ok(mock) => mock,
        Err(err) => {
            return err.to_compile_error();
        }
    };
    mock.gen()
}

/// Test cases for `mock!{}`.
#[cfg(test)]
mod t {

    use std::str::FromStr;
    use pretty_assertions::assert_eq;
    use super::super::*;

    fn check(desired: &str, code: &str) {
        let ts = proc_macro2::TokenStream::from_str(code).unwrap();
        let output = do_mock(ts).to_string();
        // Let proc_macro2 reformat the whitespace in the expected string
        let expected = proc_macro2::TokenStream::from_str(desired).unwrap()
            .to_string();
        assert_eq!(expected, output);
    }

    /// Mocking a struct with a generic method with mock!{}
    #[test]
    fn generic_method() {
        let desired = r#"
            #[derive(Default)]
            struct MockSomeStruct {
                foo: ::mockall::GenericExpectations,
            }
            impl MockSomeStruct {
                pub fn foo<T: 'static>(&self, t: T) {
                    self.foo.call:: <(T), ()>((t))
                }
                pub fn expect_foo<T: 'static>(&mut self)
                    -> &mut ::mockall::Expectation<(T), ()>
                {
                    self.foo.expect:: <(T), ()>()
                }
            }
        "#;
        let code = r#"
            SomeStruct {
                fn foo<T: 'static>(&self, t: T);
            }"#;
        check(desired, code);
    }

    /// Mocking a generic struct that's defined in another crate
    #[test]
    fn generic_struct() {
        let desired = r#"
            #[derive(Default)]
            struct MockExternalStruct<T: Clone> {
                foo: ::mockall::Expectations<(u32), i64> ,
                _t0: ::std::marker::PhantomData<T> ,
            }
            impl<T: Clone> MockExternalStruct<T> {
                pub fn foo(&self, x: u32) -> i64 {
                    self.foo.call((x))
                }
                pub fn expect_foo(&mut self)
                    -> &mut ::mockall::Expectation<(u32), i64>
                {
                    self.foo.expect()
                }
            }
        "#;
        let code = r#"
            ExternalStruct<T: Clone> {
                fn foo(&self, x: u32) -> i64;
            }
        "#;
        check(desired, code);
    }

    /// Mocking a generic struct that's defined in another crate and has a trait
    /// impl
    #[test]
    fn generic_struct_with_trait() {
        let desired = r#"
            #[derive(Default)]
            struct MockExternalStruct<T: Copy + 'static> {
                Foo_expectations: MockExternalStruct_Foo,
                _t0: ::std::marker::PhantomData<T> ,
            }
            #[derive(Default)]
            struct MockExternalStruct_Foo {
                foo: ::mockall::Expectations<(u32), u32> ,
            }
            impl<T: Copy + 'static> MockExternalStruct<T> {}
            impl<T: Copy + 'static> Foo for MockExternalStruct<T> {
                fn foo(&self, x: u32) -> u32 {
                    self.Foo_expectations.foo.call((x))
                }
            }
            impl<T: Copy + 'static> MockExternalStruct<T> {
                pub fn expect_foo(&mut self)
                    -> &mut ::mockall::Expectation<(u32), u32>
                {
                    self.Foo_expectations.foo.expect()
                }
            }
        "#;
        let code = r#"
            ExternalStruct<T: Copy + 'static> {}
            trait Foo {
                fn foo(&self, x: u32) -> u32;
            }
        "#;
        check(desired, code);
    }

    /// Implement a generic trait on a generic struct with mock!
    #[test]
    fn generic_struct_with_generic_trait() {
        let desired = r#"
            #[derive(Default)]
            struct MockExternalStruct<T: 'static, Z: 'static> {
                Foo_expectations: MockExternalStruct_Foo<T> ,
                _t0: ::std::marker::PhantomData<T> ,
                _t1: ::std::marker::PhantomData<Z> ,
            }
            #[derive(Default)]
            struct MockExternalStruct_Foo<T: 'static> {
                foo: ::mockall::Expectations<(T), T> ,
                _t0: ::std::marker::PhantomData<T> ,
            }
            impl<T: 'static, Z: 'static> MockExternalStruct<T, Z> {}
            impl<T: 'static, Z: 'static> Foo<T> for MockExternalStruct<T, Z> {
                fn foo(&self, x: T) -> T {
                    self.Foo_expectations.foo.call((x))
                }
            }
            impl<T: 'static, Z: 'static> MockExternalStruct<T, Z> {
                pub fn expect_foo(&mut self)
                    -> &mut ::mockall::Expectation<(T), T>
                {
                    self.Foo_expectations.foo.expect()
                }
            }
        "#;
        let code = r#"
            ExternalStruct<T: 'static, Z: 'static> {}
            trait Foo<T: 'static> {
                fn foo(&self, x: T) -> T;
            }
        "#;
        check(desired, code);
    }

    #[test]
    fn generic_trait() {
        let desired = r#"#[derive(Default)]
        struct MockExternalStruct<T> {
            GenericTrait_expectations: MockExternalStruct_GenericTrait<T> ,
            _t0: ::std::marker::PhantomData<T> ,
        }
        #[derive(Default)]
        struct MockExternalStruct_GenericTrait<T> {
            foo: ::mockall::Expectations<(), ()> ,
            _t0: ::std::marker::PhantomData<T> ,
        }
        impl<T> MockExternalStruct<T> {}
        impl<T> GenericTrait<T> for MockExternalStruct<T> {
            fn foo(&self) {
                self.GenericTrait_expectations.foo.call(())
            }
        }
        impl<T> MockExternalStruct<T> {
            pub fn expect_foo(&mut self) -> &mut ::mockall::Expectation<(), ()>
            {
                self.GenericTrait_expectations.foo.expect()
            }
        }"#;
        let code = r#"
        ExternalStruct<T> {}
        trait GenericTrait<T> {
            fn foo(&self);
        }"#;
        check(desired, code);
    }

    #[test]
    fn inherited_trait() {
        trait A {
            fn foo(&self);
        }
        trait B: A {
            fn bar(&self);
        }
        let desired = r#"
        #[derive(Default)]
        struct MockExternalStruct {
            A_expectations: MockExternalStruct_A,
            B_expectations: MockExternalStruct_B,
        }
        #[derive(Default)]
        struct MockExternalStruct_A {
            foo: ::mockall::Expectations<(), ()> ,
        }
        #[derive(Default)]
        struct MockExternalStruct_B {
            bar: ::mockall::Expectations<(), ()> ,
        }
        impl MockExternalStruct {}
        impl A for MockExternalStruct {
            fn foo(&self) {
                self.A_expectations.foo.call(())
            }
        }
        impl MockExternalStruct {
            pub fn expect_foo(&mut self) -> &mut ::mockall::Expectation<(), ()>
            {
                self.A_expectations.foo.expect()
            }
        }
        impl B for MockExternalStruct {
            fn bar(&self) {
                self.B_expectations.bar.call(())
            }
        }
        impl MockExternalStruct {
            pub fn expect_bar(&mut self) -> &mut ::mockall::Expectation<(), ()>
            {
                self.B_expectations.bar.expect()
            }
        }"#;
        let code = r#"
            ExternalStruct {}
            trait A {
                fn foo(&self);
            }
            trait B {
                fn bar(&self);
            }
        "#;
        check(desired, code);
    }

    #[test]
    fn reference_arguments() {
        let desired = r#"
        #[derive(Default)]
        struct MockFoo< 'a> {
            foo: ::mockall::Expectations<(& 'a u32), ()> ,
            _t0: ::std::marker::PhantomData< & 'a ()> ,
        }
        impl< 'a> MockFoo< 'a> {
            pub fn foo(&self, x: & 'a u32) {
                self.foo.call((x))
            }
            pub fn expect_foo(&mut self)
                -> &mut ::mockall::Expectation<(& 'a u32), ()>
            {
                self.foo.expect()
            }
        }"#;

        let code = r#"
            Foo<'a> {
                fn foo(&self, x: & 'a u32);
            }
        "#;
        check(desired, code);
    }

    #[test]
    fn reference_return() {
        let desired = r#"
        #[derive(Default)]
        struct MockFoo {
            foo: ::mockall::RefExpectations<(), u32> ,
        }
        impl MockFoo {
            pub fn foo(&self) -> &u32 {
                self.foo.call(())
            }
            pub fn expect_foo(&mut self)
                -> &mut ::mockall::RefExpectation<(), u32>
            {
                self.foo.expect()
            }
        }"#;

        let code = r#"
            Foo {
                fn foo(&self) -> &u32;
            }
        "#;
        check(desired, code);
    }

    #[test]
    fn reference_return_from_trait() {
        let desired = r#"
        #[derive(Default)]
        struct MockX {
            Foo_expectations: MockX_Foo ,
        }
        #[derive(Default)]
        struct MockX_Foo {
            foo: ::mockall::RefExpectations<(), u32> ,
        }
        impl MockX {}
        impl Foo for MockX {
            fn foo(&self) -> &u32 {
                self.Foo_expectations.foo.call(())
            }
        }
        impl MockX {
            pub fn expect_foo(&mut self)
                -> &mut ::mockall::RefExpectation<(), u32>
            {
                self.Foo_expectations.foo.expect()
            }
        }"#;

        let code = r#"
            X {}
            trait Foo {
                fn foo(&self) -> &u32;
            }
        "#;
        check(desired, code);
    }

    #[test]
    fn ref_mut_return() {
        let desired = r#"
        #[derive(Default)]
        struct MockFoo {
            foo: ::mockall::RefMutExpectations<(), u32> ,
        }
        impl MockFoo {
            pub fn foo(&mut self) -> &mut u32 {
                self.foo.call_mut(())
            }
            pub fn expect_foo(&mut self)
                -> &mut ::mockall::RefMutExpectation<(), u32>
            {
                self.foo.expect()
            }
        }"#;

        let code = r#"
            Foo {
                fn foo(&mut self) -> &mut u32;
            }
        "#;
        check(desired, code);
    }

    #[test]
    fn static_method() {
        let desired = r#"
            #[derive(Default)]
            struct MockFoo {
            }
            ::mockall::lazy_static!{
                static ref MockFoo_bar_expectation: ::std::sync::Mutex< ::mockall::Expectations<(u32), u64> >
                = ::std::sync::Mutex::new(::mockall::Expectations::new());
            }
            impl MockFoo {
                pub fn bar(x: u32) -> u64 {
                    MockFoo_bar_expectation.lock().unwrap().call((x))
                }
                pub fn expect_bar< 'guard>()
                    -> ::mockall::ExpectationGuard< 'guard, (u32), u64>
                {
                    ::mockall::ExpectationGuard::new(
                        MockFoo_bar_expectation.lock().unwrap()
                    )
                }
            }
        "#;
        let code = r#"
            Foo {
                fn bar(x: u32) -> u64;
            }
        "#;
        check(desired, code);
    }

    /// Mocking a struct that's defined in another crate with mock!
    #[test]
    fn struct_() {
        let desired = r#"
            #[derive(Default)]
            struct MockExternalStruct {
                foo: ::mockall::Expectations<(u32), i64> ,
            }
            impl MockExternalStruct {
                pub fn foo(&self, x: u32) -> i64 {
                    self.foo.call((x))
                }
                pub fn expect_foo(&mut self)
                    -> &mut ::mockall::Expectation<(u32), i64>
                {
                    self.foo.expect()
                }
            }
        "#;
        let code = r#"
            ExternalStruct {
                fn foo(&self, x: u32) -> i64;
            }
        "#;
        check(desired, code);
    }

    /// Mocking a struct that's defined in another crate, and has a trait
    /// implementation
    #[test]
    fn struct_with_trait() {
        let desired = r#"
            #[derive(Default)]
            struct MockExternalStruct {
                Foo_expectations: MockExternalStruct_Foo,
            }
            #[derive(Default)]
            struct MockExternalStruct_Foo {
                foo: ::mockall::Expectations<(u32), i64> ,
            }
            impl MockExternalStruct { }
            impl Foo for MockExternalStruct {
                fn foo(&self, x: u32) -> i64 {
                    self.Foo_expectations.foo.call((x))
                }
            }
            impl MockExternalStruct {
                pub fn expect_foo(&mut self)
                    -> &mut ::mockall::Expectation<(u32), i64>
                {
                    self.Foo_expectations.foo.expect()
                }
            }
        "#;
        let code = r#"
            ExternalStruct {}
            trait Foo {
                fn foo(&self, x: u32) -> i64;
            }
        "#;
        check(desired, code);
    }

    /// Mocking a struct that's defined in another crate, and has a trait
    /// implementation that includes an associated type
    #[test]
    fn struct_with_trait_with_associated_types() {
        let desired = r#"
            #[derive(Default)]
            struct MockMyIter {
                Iterator_expectations: MockMyIter_Iterator,
            }
            #[derive(Default)]
            struct MockMyIter_Iterator {
                next: ::mockall::Expectations<(), Option<u32> > ,
            }
            impl MockMyIter { }
            impl Iterator for MockMyIter {
                type Item=u32;
                fn next(&mut self) -> Option<u32> {
                    self.Iterator_expectations.next.call(())
                }
            }
            impl MockMyIter {
                pub fn expect_next(&mut self)
                    -> &mut ::mockall::Expectation<(), Option<u32> >
                {
                    self.Iterator_expectations.next.expect()
                }
            }
        "#;
        let code = r#"
            MyIter {}
            trait Iterator {
                type Item=u32;

                fn next(&mut self) -> Option<u32>;
            }
        "#;
        check(desired, code);
    }

}
