// SPDX-License-Identifier: Apache-2.0

//! `NodeDbMarker` — platform-specific `Send + Sync` bound for the `NodeDb` trait.

/// Marker bound for `NodeDb` and the futures it returns.
///
/// On native targets the bound is `Send + Sync` — matching the multi-thread
/// Tokio runtime that backs both Origin and the desktop / mobile-FFI Lite
/// callers. On `wasm32` the bound is empty: JS is single-threaded, so
/// requiring `Send` on futures returned by the trait would force every
/// `!Send` engine internal (redb transactions, `Rc<...>`, etc.) to be
/// rewritten for no benefit.
///
/// The `#[async_trait]` attribute on the trait + each impl is correspondingly
/// cfg-swapped between the default (`Send` futures) and `?Send` (no `Send`
/// bound) variants.
#[cfg(not(target_arch = "wasm32"))]
pub trait NodeDbMarker: Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync + ?Sized> NodeDbMarker for T {}

#[cfg(target_arch = "wasm32")]
pub trait NodeDbMarker {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> NodeDbMarker for T {}
