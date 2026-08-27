// Copyright 2026 The AccessKit Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0 (found in
// the LICENSE-APACHE file) or the MIT license (found in
// the LICENSE-MIT file), at your option.

#[cfg(any(test, all(target_arch = "wasm32", target_os = "unknown")))]
mod geometry;
#[cfg(any(test, all(target_arch = "wasm32", target_os = "unknown")))]
mod roles;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod adapter;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use adapter::Adapter;
