//! Procedural macros for telepatch (`sh!`, `#[role]`). Re-exported by the
//! `telepatch` crate; depend on that instead of this one.

use proc_macro::TokenStream;

/// Expands to the string `"telepatch-macros"`. Used by smoke tests to prove the re-export path.
#[doc(hidden)]
#[proc_macro]
pub fn __macros_marker(_input: TokenStream) -> TokenStream {
    quote::quote!("telepatch-macros").into()
}
