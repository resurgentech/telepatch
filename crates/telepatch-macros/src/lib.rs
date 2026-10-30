//! Procedural macros for telepatch (`sh!`, `#[role]`). Re-exported by the
//! `telepatch` crate; depend on that instead of this one.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Expr, ExprLit, FnArg, Ident, ItemFn, Lit, LitStr, Pat, Token};

/// Expands to the string `"telepatch-macros"`. Used by smoke tests to prove the re-export path.
#[doc(hidden)]
#[proc_macro]
pub fn __macros_marker(_input: TokenStream) -> TokenStream {
    quote!("telepatch-macros").into()
}

struct ShInput {
    host: Expr,
    cmd: LitStr,
    unless: Option<LitStr>,
    creates: Option<LitStr>,
    changed_if: Option<LitStr>,
}

fn string_lit(e: Expr, what: &str) -> syn::Result<LitStr> {
    match e {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => Ok(s),
        other => Err(syn::Error::new_spanned(
            other,
            format!("{what} must be a string literal"),
        )),
    }
}

impl Parse for ShInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let host: Expr = input.parse()?;
        input.parse::<Token![,]>()?;
        let cmd = string_lit(input.parse()?, "the sh! command (format string)")?;
        let mut out = ShInput {
            host,
            cmd,
            unless: None,
            creates: None,
            changed_if: None,
        };
        while input.parse::<Option<Token![,]>>()?.is_some() {
            if input.is_empty() {
                break;
            }
            let name: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let value = string_lit(input.parse()?, &format!("`{name}`"))?;
            let slot = match name.to_string().as_str() {
                "unless" => &mut out.unless,
                "creates" => &mut out.creates,
                "changed_if" => &mut out.changed_if,
                _ => {
                    return Err(syn::Error::new(
                        name.span(),
                        format!(
                            "unknown sh! argument `{name}`; expected `unless`, `creates` or `changed_if`"
                        ),
                    ));
                }
            };
            if slot.is_some() {
                return Err(syn::Error::new(
                    name.span(),
                    format!("`{name}` given twice"),
                ));
            }
            *slot = Some(value);
        }
        Ok(out)
    }
}

/// Turn a literal with `{ident}` placeholders into a `format!` call whose
/// arguments are the shell-quoted `Display` output of each identifier.
/// `{{` and `}}` are literal braces.
fn interpolate(lit: &LitStr) -> syn::Result<TokenStream2> {
    let src = lit.value();
    let mut fmt = String::new();
    let mut args: Vec<TokenStream2> = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                fmt.push_str("{{");
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                fmt.push_str("}}");
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => {
                            return Err(syn::Error::new(
                                lit.span(),
                                "unclosed `{` in sh! string (write `{{` for a literal brace)",
                            ));
                        }
                    }
                }
                let ident = syn::parse_str::<Ident>(&name).map_err(|_| {
                    syn::Error::new(
                        lit.span(),
                        format!(
                            "unsupported placeholder `{{{name}}}`: only `{{identifier}}` is allowed (write `{{{{` for a literal brace)"
                        ),
                    )
                })?;
                let ident = Ident::new(&ident.to_string(), lit.span());
                fmt.push_str("{}");
                args.push(quote! {
                    ::telepatch::sh::quote(&::std::string::ToString::to_string(&#ident))
                });
            }
            '}' => {
                return Err(syn::Error::new(
                    lit.span(),
                    "unmatched `}` in sh! string (write `}}` for a literal brace)",
                ));
            }
            c => fmt.push(c),
        }
    }
    Ok(quote! { ::std::format!(#fmt #(, #args)*) })
}

fn opt(l: &Option<LitStr>) -> syn::Result<TokenStream2> {
    Ok(match l {
        Some(l) => {
            let e = interpolate(l)?;
            quote! { ::std::option::Option::Some(#e) }
        }
        None => quote! { ::std::option::Option::None },
    })
}

/// `sh!(host, "cmd {var}", unless = "..", creates = "..", changed_if = "..")`
///
/// Appends a shell op to the host's trace and returns its `OpHandle`.
/// `{var}` interpolates an in-scope variable, shell-quoted; `{{`/`}}` are
/// literal braces. The named arguments interpolate the same way.
#[proc_macro]
pub fn sh(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as ShInput);
    let go = || -> syn::Result<TokenStream2> {
        let host = &input.host;
        let cmd = interpolate(&input.cmd)?;
        let unless = opt(&input.unless)?;
        let creates = opt(&input.creates)?;
        let changed_if = opt(&input.changed_if)?;
        Ok(quote! {
            ::telepatch::sh::shell(&(#host), ::telepatch::sh::ShellSpec {
                cmd: #cmd,
                unless: #unless,
                creates: #creates,
                changed_if: #changed_if,
            })
        })
    };
    go().unwrap_or_else(|e| e.to_compile_error()).into()
}

/// Mark an `async fn name(h: &Host<V>)` (unit or `Result<(), TraceError>`)
/// as a role: its body is bracketed by `RoleBegin`/`RoleEnd` (also on early
/// return), and a hidden `const __tp_role_name_<fn>: &str` carries the name.
#[proc_macro_attribute]
pub fn role(args: TokenStream, item: TokenStream) -> TokenStream {
    if !args.is_empty() {
        let a = TokenStream2::from(args);
        return syn::Error::new_spanned(a, "#[role] takes no arguments")
            .to_compile_error()
            .into();
    }
    let mut f = syn::parse_macro_input!(item as ItemFn);
    match expand_role(&mut f) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_role(f: &mut ItemFn) -> syn::Result<TokenStream2> {
    if f.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            f.sig.fn_token,
            "a role must be an `async fn`",
        ));
    }
    let host = match f.sig.inputs.first() {
        Some(FnArg::Typed(t)) => match &*t.pat {
            Pat::Ident(p) => p.ident.clone(),
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "the first parameter of a role must be a plain identifier (the host)",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                &f.sig,
                "a role takes the host as its first parameter: `async fn name(h: &Host<V>)`",
            ));
        }
    };
    let name = f.sig.ident.to_string();
    let block = &f.block;
    let new_block: syn::Block = syn::parse_quote_spanned! { Span::call_site() =>
        {
            let __tp_role = #host.role_enter(#name);
            #block
        }
    };
    *f.block = new_block;
    let konst = format_ident!("__tp_role_name_{}", f.sig.ident);
    let vis = &f.vis;
    Ok(quote! {
        #f
        #[doc(hidden)]
        #[allow(non_upper_case_globals, dead_code)]
        #vis const #konst: &str = #name;
    })
}
