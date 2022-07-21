use proc_macro2::{Span, TokenStream};
use quote::quote;
use std::path::Path;
use syn::LitStr;

struct Args {
    fixtures: Vec<LitStr>,
    migrations: MigrationsOpt,
}

enum MigrationsOpt {
    InferredPath,
    ExplicitPath(LitStr),
    Disabled,
}

pub fn expand(args: syn::AttributeArgs, input: syn::ItemFn) -> syn::Result<TokenStream> {
    let ret = &input.sig.output;
    let name = &input.sig.ident;
    let inputs = &input.sig.inputs;
    let body = &input.block;
    let attrs = &input.attrs;

    let args = parse_args(args)?;

    let fixtures = args
        .fixtures
        .into_iter()
        .map(|fixture| {
            let path = Path::new("fixtures").join(fixture.value());
            let path = crate::common::resolve_path(&path, fixture.span())?
                .display()
                .to_string();

            Ok(quote! {
                ::sqlx::testing::TestFixture {
                    path: #path,
                    contents: include_str!(#path),
                }
            })
        })
        .collect::<syn::Result<Vec<_>>>()?;

    let migrations = match args.migrations {
        MigrationsOpt::ExplicitPath(path) => {
            let migrator = crate::migrate::expand_migrator_from_lit_dir(path)?;
            quote! { args.migrator(#migrator); }
        }
        MigrationsOpt::InferredPath => {
            let migrator =
                crate::migrate::expand_migrator_from_dir("migrations", Span::call_site());
            quote! { args.migrator(#migrator); }
        }
        MigrationsOpt::Disabled => quote! {},
    };

    Ok(quote! {
        #[test]
        #(#attrs)*
        fn #name() #ret {
            async fn inner(#inputs) #ret {
                #body
            }

            let mut args = ::sqlx::testing::TestArgs::new(concat!(module_path!(), "::", stringify!(#name)));

            #migrations

            args.fixtures(&[#(#fixtures),*]);

            ::sqlx::testing::TestFn::run_test(inner, args)
        }
    })
}

fn parse_args(attr_args: syn::AttributeArgs) -> syn::Result<Args> {
    let mut fixtures = vec![];
    let mut migrations = MigrationsOpt::InferredPath;

    for arg in attr_args {
        match arg {
            syn::NestedMeta::Meta(syn::Meta::List(list)) if list.path.is_ident("fixtures") => {
                if !fixtures.is_empty() {
                    return Err(syn::Error::new_spanned(list, "duplicate `fixtures` arg"));
                }

                for nested in list.nested {
                    match nested {
                        syn::NestedMeta::Lit(syn::Lit::Str(litstr)) => fixtures.push(litstr),
                        other => {
                            return Err(syn::Error::new_spanned(other, "expected string literal"))
                        }
                    }
                }
            }
            syn::NestedMeta::Meta(syn::Meta::NameValue(namevalue))
                if namevalue.path.is_ident("migrations") =>
            {
                if !matches!(migrations, MigrationsOpt::InferredPath) {
                    return Err(syn::Error::new_spanned(
                        namevalue,
                        "duplicate `migrations` arg",
                    ));
                }

                migrations = match namevalue.lit {
                    syn::Lit::Bool(litbool) => {
                        if !litbool.value {
                            // migrations = false
                            MigrationsOpt::Disabled
                        } else {
                            // migrations = true
                            return Err(syn::Error::new_spanned(
                                litbool,
                                "`migrations = true` is redundant",
                            ));
                        }
                    }
                    // migrations = "<path>"
                    syn::Lit::Str(litstr) => MigrationsOpt::ExplicitPath(litstr),
                    _ => {
                        return Err(syn::Error::new_spanned(
                            namevalue,
                            "expected string or `false`",
                        ))
                    }
                };
            }
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "expected `fixtures(\"<filename>\", ...)` or `migrations = \"<path>\" | false`",
                ))
            }
        }
    }

    Ok(Args {
        fixtures,
        migrations,
    })
}
