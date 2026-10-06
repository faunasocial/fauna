class ConcurrentHandleMap<T> where T: notnull {
    Dictionary<ulong, T> map = new Dictionary<ulong, T>();

    Object lock_ = new Object();
    // Foreign handles must be odd so the Rust side can distinguish them from Rust-owned pointers.
    // See: https://mozilla.github.io/uniffi-rs/latest/internals/object_references.html
    ulong currentHandle = 1;

    public ulong Insert(T obj) {
        lock (lock_) {
            var handle = currentHandle;
            // Increment by 2 to keep handles odd.
            currentHandle += 2;
            map[handle] = obj;
            return handle;
        }
    }

    public bool TryGet(ulong handle, out T result) {
        lock (lock_) {
            #pragma warning disable 8601 // Possible null reference assignment
            return map.TryGetValue(handle, out result);
            #pragma warning restore 8601
        }
    }

    public T Get(ulong handle) {
        if (TryGet(handle, out var result)) {
            return result;
        } else {
            throw new InternalException("ConcurrentHandleMap: Invalid handle");
        }
    }

    public bool Remove(ulong handle) {
        return Remove(handle, out T result);
    }

    public bool Remove(ulong handle, out T result) {
        lock (lock_) {
            // Possible null reference assignment
            #pragma warning disable 8601
            if (map.TryGetValue(handle, out result)) {
            #pragma warning restore 8601
                map.Remove(handle);
                return true;
            } else {
                return false;
            }
        }
    }
}
