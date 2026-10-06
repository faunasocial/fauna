{#/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */#}

// Per-crate Alloc/Free for the *shared* `uniffi.RustBuffer` struct. Each
// uniffi crate exports its own `ffi_<crate>_rustbuffer_alloc/free` symbols
// (different symbols, same cdylib), so the dispatch lives here and not in
// the shared runtime.

internal static class _UniffiRustBufferOps {
    public static RustBuffer Alloc(int size) {
        return _UniffiHelpers.RustCall((ref UniffiRustCallStatus status) => {
            var buffer = _UniFFILib.{{ ci.ffi_rustbuffer_alloc().name() }}(Convert.ToUInt64(size), ref status);
            if (buffer.data == IntPtr.Zero) {
                throw new AllocationException($"_UniffiRustBufferOps.Alloc() returned null data pointer (size={size})");
            }
            return buffer;
        });
    }

    public static void Free(RustBuffer buffer) {
        _UniffiHelpers.RustCall((ref UniffiRustCallStatus status) => {
            _UniFFILib.{{ ci.ffi_rustbuffer_free().name() }}(buffer, ref status);
        });
    }
}
