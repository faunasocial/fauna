/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Crate-agnostic FFI runtime shared by every generated `uniffi.<crate>.cs`
// file. Equivalent to what Kotlin/Swift get for free from JVM/Foundation —
// types that cross UniFFI namespace boundaries (BigEndianStream, RustBuffer,
// UniffiRustCallStatus, the exception hierarchy) MUST be a single C# type
// across all files for cross-crate `FfiConverter` calls to type-check.
//
// Per-crate files emit `using uniffi;` so their local code resolves these
// names without qualification.

using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;

namespace uniffi;

// Big endian streams are not yet available in dotnet :'(
// https://github.com/dotnet/runtime/issues/26904

internal class StreamUnderflowException: System.Exception {
    public StreamUnderflowException() {
    }
}

internal static class BigEndianStreamExtensions
{
    public static void WriteInt32(this Stream stream, int value, int bytesToWrite = 4)
    {
        Span<byte> buffer = stackalloc byte[bytesToWrite];
        var posByte = bytesToWrite;
        while (posByte != 0)
        {
            posByte--;
            buffer[posByte] = (byte)(value);
            value >>= 8;
        }
        stream.Write(buffer);
    }

    public static void WriteInt64(this Stream stream, long value)
    {
        int bytesToWrite = 8;
        Span<byte> buffer = stackalloc byte[bytesToWrite];
        var posByte = bytesToWrite;
        while (posByte != 0)
        {
            posByte--;
            buffer[posByte] = (byte)(value);
            value >>= 8;
        }
        stream.Write(buffer);
    }

    public static uint ReadUint32(this Stream stream, int bytesToRead = 4) {
        CheckRemaining(stream, bytesToRead);
        Span<byte> buffer = stackalloc byte[bytesToRead];
        stream.Read(buffer);
        uint result = 0;
        uint digitMultiplier = 1;
        int posByte = bytesToRead;
        while (posByte != 0)
        {
            posByte--;
            result |= buffer[posByte]*digitMultiplier;
            digitMultiplier <<= 8;
        }
        return result;
    }

    public static ulong ReadUInt64(this Stream stream) {
        int bytesToRead = 8;
        CheckRemaining(stream, bytesToRead);
        Span<byte> buffer = stackalloc byte[bytesToRead];
        stream.Read(buffer);
        ulong result = 0;
        ulong digitMultiplier = 1;
        int posByte = bytesToRead;
        while (posByte != 0)
        {
            posByte--;
            result |= buffer[posByte]*digitMultiplier;
            digitMultiplier <<= 8;
        }
        return result;
    }

    public static void CheckRemaining(this Stream stream, int length) {
        if (stream.Length - stream.Position < length) {
            throw new StreamUnderflowException();
        }
    }

    public static void ForEach<T>(this T[] items, Action<T> action){
        foreach (var item in items) {
            action(item);
        }
    }
}

internal class BigEndianStream {
    Stream stream;
    public BigEndianStream(Stream stream) {
        this.stream = stream;
    }

    public bool HasRemaining() {
        return (stream.Length - Position) > 0;
    }

    public long Position {
        get => stream.Position;
        set => stream.Position = value;
    }

    public void WriteBytes(byte[] buffer) {
        stream.Write(buffer);
    }

    public void WriteByte(byte value) => stream.WriteInt32(value, bytesToWrite: 1);
    public void WriteSByte(sbyte value) => stream.WriteInt32(value, bytesToWrite: 1);

    public void WriteUShort(ushort value) => stream.WriteInt32(value, bytesToWrite: 2);
    public void WriteShort(short value) => stream.WriteInt32(value, bytesToWrite: 2);

    public void WriteUInt(uint value) => stream.WriteInt32((int)value);
    public void WriteInt(int value) => stream.WriteInt32(value);

    public void WriteULong(ulong value) => stream.WriteInt64((long)value);
    public void WriteLong(long value) => stream.WriteInt64(value);

    public void WriteFloat(float value) {
        unsafe {
            WriteInt(*((int*)&value));
        }
    }
    public void WriteDouble(double value) => stream.WriteInt64(BitConverter.DoubleToInt64Bits(value));

    public byte[] ReadBytes(int length) {
        stream.CheckRemaining(length);
        byte[] result = new byte[length];
        stream.Read(result, 0, length);
        return result;
    }

    public byte ReadByte() => (byte)stream.ReadUint32(bytesToRead: 1);
    public ushort ReadUShort() => (ushort)stream.ReadUint32(bytesToRead: 2);
    public uint ReadUInt() => (uint)stream.ReadUint32(bytesToRead: 4);
    public ulong ReadULong() => stream.ReadUInt64();

    public sbyte ReadSByte() => (sbyte)ReadByte();
    public short ReadShort() => (short)ReadUShort();
    public int ReadInt() => (int)ReadUInt();

    public float ReadFloat() {
        unsafe {
            int value = ReadInt();
            return *((float*)&value);
        }
    }

    public long ReadLong() => (long)ReadULong();
    public double ReadDouble() => BitConverter.Int64BitsToDouble(ReadLong());
}

// `RustBuffer` represents a UniFFI rust-allocated buffer (capacity, len, data
// pointer). The struct *layout* is shared across every `uniffi.<crate>` so
// the FFI ABI matches; per-crate `_UniffiRustBufferOps.Alloc/Free` performs
// the actual allocator dispatch (different `ffi_<crate>_rustbuffer_alloc`
// symbols, all in the same cdylib).
[StructLayout(LayoutKind.Sequential)]
internal struct RustBuffer {
    public ulong capacity;
    public ulong len;
    public IntPtr data;

    public static BigEndianStream MemoryStream(IntPtr data, long length)
    {
        unsafe
        {
            return new BigEndianStream(new UnmanagedMemoryStream((byte*)data.ToPointer(), length));
        }
    }

    public BigEndianStream AsStream()
    {
        unsafe
        {
            return new BigEndianStream(
                new UnmanagedMemoryStream((byte*)data.ToPointer(), Convert.ToInt64(len))
            );
        }
    }

    public BigEndianStream AsWriteableStream()
    {
        unsafe
        {
            return new BigEndianStream(
                new UnmanagedMemoryStream(
                    (byte*)data.ToPointer(),
                    Convert.ToInt64(capacity),
                    Convert.ToInt64(capacity),
                    FileAccess.Write
                )
            );
        }
    }
}

// Helper for safely passing byte references into rust code.
[StructLayout(LayoutKind.Sequential)]
internal struct ForeignBytes {
    public int length;
    public IntPtr data;
}

// Result of a FFI call. Per-crate `_UniffiHelpers.RustCall*` interpret the
// status code and lift the error buffer.
[StructLayout(LayoutKind.Sequential)]
internal struct UniffiRustCallStatus {
    public sbyte code;
    public RustBuffer error_buf;

    public bool IsSuccess() {
        return code == 0;
    }

    public bool IsError() {
        return code == 1;
    }

    public bool IsPanic() {
        return code == 2;
    }
}

// Base class for all uniffi exceptions. Shared so a `catch (UniffiException)`
// in any namespace catches errors raised by sibling namespaces' converters.
internal class UniffiException: System.Exception {
    public UniffiException(): base() {}
    public UniffiException(string message): base(message) {}
}

internal class UndeclaredErrorException: UniffiException {
    public UndeclaredErrorException(string message): base(message) {}
}

internal class PanicException: UniffiException {
    public PanicException(string message): base(message) {}
}

internal class AllocationException: UniffiException {
    public AllocationException(string message): base(message) {}
}

internal class InternalException: UniffiException {
    public InternalException(string message): base(message) {}
}

internal class InvalidEnumException: InternalException {
    public InvalidEnumException(string message): base(message) {
    }
}

internal class UniffiContractVersionException: UniffiException {
    public UniffiContractVersionException(string message): base(message) {
    }
}

internal class UniffiContractChecksumException: UniffiException {
    public UniffiContractChecksumException(string message): base(message) {
    }
}

// Each top-level error class has a companion object that can lift the error
// from the call status's rust buffer.
internal interface CallStatusErrorHandler<E> where E: System.Exception {
    E Lift(RustBuffer error_buf);
}

internal static class FFIObjectUtil {
    public static void DisposeAll(params Object?[] list) {
        Dispose(list);
    }

    // Dispose is implemented by recursive type inspection at runtime. This is because
    // generating correct Dispose calls for recursive complex types, e.g. List<List<int>>
    // is quite cumbersome.
    private static void Dispose(Object? obj) {
        if (obj == null) {
            return;
        }

        if (obj is IDisposable disposable) {
            disposable.Dispose();
            return;
        }

        var objType = obj.GetType();
        var typeCode = Type.GetTypeCode(objType);
        if (typeCode != TypeCode.Object) {
            return;
        }

        var genericArguments = objType.GetGenericArguments();
        if (genericArguments.Length == 0 && !objType.IsArray) {
            return;
        }

        if (obj is System.Collections.IDictionary objDictionary) {
            //This extra code tests to not call "Dispose" for a Dictionary<something, double>()
            //for all values as "double" and alike doesn't support interface "IDisposable"
            var valuesType = objType.GetGenericArguments()[1];
            var elementValuesTypeCode = Type.GetTypeCode(valuesType);
            if (elementValuesTypeCode != TypeCode.Object) {
                return;
            }
            foreach (var value in objDictionary.Values) {
                Dispose(value);
            }
        }
        else if (obj is System.Collections.IEnumerable listValues) {
            //This extra code tests to not call "Dispose" for a List<int>()
            //for all keys as "int" and alike doesn't support interface "IDisposable"
            var elementType = objType.IsArray ? objType.GetElementType() : genericArguments[0];
            var elementValuesTypeCode = Type.GetTypeCode(elementType);
            if (elementValuesTypeCode != TypeCode.Object) {
                return;
            }
            foreach (var value in listValues) {
                Dispose(value);
            }
        }
    }
}
