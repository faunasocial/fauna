{{ self.add_import("System.Threading")}}
{{ self.add_import("System.Threading.Tasks")}}

{% if self.include_once_check("ConcurrentHandleMap.cs") %}{% include "ConcurrentHandleMap.cs" %}{% endif %}

[UnmanagedFunctionPointer(CallingConvention.Cdecl)]
delegate void UniFfiFutureCallback(IntPtr continuationHandle, byte pollResult);

internal static class _UniFFIAsync {
    internal const byte UNIFFI_RUST_FUTURE_POLL_READY = 0;
    // internal const byte UNIFFI_RUST_FUTURE_POLL_MAYBE_READY = 1;

    internal static ConcurrentHandleMap<TaskCompletionSource<byte>> _async_handle_map = new ConcurrentHandleMap<TaskCompletionSource<byte>>();
    public static ConcurrentHandleMap<CancellationTokenSource> _foreign_futures_map = new ConcurrentHandleMap<CancellationTokenSource>();

    // FFI type for Rust future continuations
    internal class UniffiRustFutureContinuationCallback
    {
        public static UniFfiFutureCallback callback = Callback;

        public static void Callback(IntPtr continuationHandle, byte pollResult)
        {
            if (_async_handle_map.Remove((ulong)continuationHandle.ToInt64(), out TaskCompletionSource<byte> task))
            {
                task.SetResult(pollResult);
            }
            else 
            {
                throw new InternalException($"Unable to find continuation handle: {continuationHandle}");
            }
        }
    }

    public class UniffiForeignFutureDroppedCallback
    {
        public static _UniFFILib.UniffiForeignFutureDroppedCallback callback = Callback;

        public static void Callback(ulong handle)
        {
            if (_foreign_futures_map.Remove(handle, out CancellationTokenSource task))
            {
                task.Cancel();
            }
            else
            {
                throw new InternalException($"Unable to find cancellation token: {handle}");
            }
        }
    }

    public delegate F CompleteFuncDelegate<F>(IntPtr ptr, ref UniffiRustCallStatus status);

    public delegate void CompleteActionDelegate(IntPtr ptr, ref UniffiRustCallStatus status);

    // ⚠ ConfigureAwait(false) on every await in this class is LOAD-BEARING, not
    // style. Only the FIRST rust_future_poll runs on the caller's thread (it is
    // synchronous inside pollFunc); polls 2..N resume from this await. Capture
    // the caller's SynchronizationContext and every one of them is queued back
    // onto that context — so on a single-threaded UI context (WinUI's
    // DispatcherQueueSynchronizationContext, and WPF/WinForms alike) a call
    // begun on the UI thread cannot advance while that thread is busy: the Rust
    // future has been entered and has set whatever in-flight flag it keeps, but
    // its request is never issued and its response is never consumed. Windows
    // e2e `test_bundled_provider.py` stalled at a different step in seven runs
    // running on exactly this, under UIA calls holding the UI thread 17-25 s.
    //
    // Dropping the context here does NOT move the caller's own continuation:
    // the app's `await machine.DoThing()` still captures the UI context and
    // still resumes on the UI thread, which is what a ViewModel needs. Only the
    // poll loop underneath it goes context-free. Pinned by
    // FaunaApp.Tests.UniffiAsyncOffUiThreadTests.
    private static async Task PollFuture(IntPtr rustFuture, Action<IntPtr, IntPtr, IntPtr> pollFunc)
    {
        byte pollResult;
        do
        {
            var tcs = new TaskCompletionSource<byte>(TaskCreationOptions.RunContinuationsAsynchronously);
            IntPtr callback = Marshal.GetFunctionPointerForDelegate(UniffiRustFutureContinuationCallback.callback);
            ulong mapEntry = _async_handle_map.Insert(tcs);
            pollFunc(rustFuture, callback, (IntPtr)mapEntry);
            pollResult = await tcs.Task.ConfigureAwait(false);
        }
        while(pollResult != UNIFFI_RUST_FUTURE_POLL_READY);
    }

    public static async Task<T> UniffiRustCallAsync<T, F, E>(
        IntPtr rustFuture,
        Action<IntPtr, IntPtr, IntPtr> pollFunc,
        CompleteFuncDelegate<F> completeFunc,
        Action<IntPtr> freeFunc,
        Func<F, T> liftFunc,
        CallStatusErrorHandler<E> errorHandler
    ) where E : UniffiException
    {
        try {
            await PollFuture(rustFuture, pollFunc).ConfigureAwait(false);
            var result = _UniffiHelpers.RustCallWithError(errorHandler, (ref UniffiRustCallStatus status) => completeFunc(rustFuture, ref status));
            return liftFunc(result);
        }
        finally
        {
            freeFunc(rustFuture);
        }
    }

    public static async Task UniffiRustCallAsync<E>(
        IntPtr rustFuture,
        Action<IntPtr, IntPtr, IntPtr> pollFunc,
        CompleteActionDelegate completeFunc,
        Action<IntPtr> freeFunc,
        CallStatusErrorHandler<E> errorHandler
    ) where E : UniffiException
    {
         try {
            await PollFuture(rustFuture, pollFunc).ConfigureAwait(false);
            _UniffiHelpers.RustCallWithError(errorHandler, (ref UniffiRustCallStatus status) => completeFunc(rustFuture, ref status));

        }
        finally
        {
            freeFunc(rustFuture);
        }
    }
}