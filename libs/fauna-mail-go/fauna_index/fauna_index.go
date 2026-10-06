package fauna_index

// #include <fauna_index.h>
import "C"

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"io"
	"math"
	"runtime"
	"sync/atomic"
	"unsafe"
)

// This is needed, because as of go 1.24
// type RustBuffer C.RustBuffer cannot have methods,
// RustBuffer is treated as non-local type
type GoRustBuffer struct {
	inner C.RustBuffer
}

type RustBufferI interface {
	AsReader() *bytes.Reader
	Free()
	ToGoBytes() []byte
	Data() unsafe.Pointer
	Len() uint64
	Capacity() uint64
}

// C.RustBuffer fields exposed as an interface so they can be accessed in different Go packages.
// See https://github.com/golang/go/issues/13467
type ExternalCRustBuffer interface {
	Data() unsafe.Pointer
	Len() uint64
	Capacity() uint64
}

func RustBufferFromC(b C.RustBuffer) ExternalCRustBuffer {
	return GoRustBuffer{
		inner: b,
	}
}

func CFromRustBuffer(b ExternalCRustBuffer) C.RustBuffer {
	return C.RustBuffer{
		capacity: C.uint64_t(b.Capacity()),
		len:      C.uint64_t(b.Len()),
		data:     (*C.uchar)(b.Data()),
	}
}

func RustBufferFromExternal(b ExternalCRustBuffer) GoRustBuffer {
	return GoRustBuffer{
		inner: C.RustBuffer{
			capacity: C.uint64_t(b.Capacity()),
			len:      C.uint64_t(b.Len()),
			data:     (*C.uchar)(b.Data()),
		},
	}
}

func (cb GoRustBuffer) Capacity() uint64 {
	return uint64(cb.inner.capacity)
}

func (cb GoRustBuffer) Len() uint64 {
	return uint64(cb.inner.len)
}

func (cb GoRustBuffer) Data() unsafe.Pointer {
	return unsafe.Pointer(cb.inner.data)
}

func (cb GoRustBuffer) AsReader() *bytes.Reader {
	b := unsafe.Slice((*byte)(cb.inner.data), C.uint64_t(cb.inner.len))
	return bytes.NewReader(b)
}

func (cb GoRustBuffer) Free() {
	rustCall(func(status *C.RustCallStatus) bool {
		C.ffi_fauna_index_rustbuffer_free(cb.inner, status)
		return false
	})
}

func (cb GoRustBuffer) ToGoBytes() []byte {
	return C.GoBytes(unsafe.Pointer(cb.inner.data), C.int(cb.inner.len))
}

func stringToRustBuffer(str string) C.RustBuffer {
	return bytesToRustBuffer([]byte(str))
}

func bytesToRustBuffer(b []byte) C.RustBuffer {
	if len(b) == 0 {
		return C.RustBuffer{}
	}
	// We can pass the pointer along here, as it is pinned
	// for the duration of this call
	foreign := C.ForeignBytes{
		len:  C.int(len(b)),
		data: (*C.uchar)(unsafe.Pointer(&b[0])),
	}

	return rustCall(func(status *C.RustCallStatus) C.RustBuffer {
		return C.ffi_fauna_index_rustbuffer_from_bytes(foreign, status)
	})
}

type BufLifter[GoType any] interface {
	Lift(value RustBufferI) GoType
}

type BufLowerer[GoType any] interface {
	Lower(value GoType) C.RustBuffer
}

type BufReader[GoType any] interface {
	Read(reader io.Reader) GoType
}

type BufWriter[GoType any] interface {
	Write(writer io.Writer, value GoType)
}

func LowerIntoRustBuffer[GoType any](bufWriter BufWriter[GoType], value GoType) C.RustBuffer {
	// This might be not the most efficient way but it does not require knowing allocation size
	// beforehand
	var buffer bytes.Buffer
	bufWriter.Write(&buffer, value)

	bytes, err := io.ReadAll(&buffer)
	if err != nil {
		panic(fmt.Errorf("reading written data: %w", err))
	}
	return bytesToRustBuffer(bytes)
}

func LiftFromRustBuffer[GoType any](bufReader BufReader[GoType], rbuf RustBufferI) GoType {
	defer rbuf.Free()
	reader := rbuf.AsReader()
	item := bufReader.Read(reader)
	if reader.Len() > 0 {
		// TODO: Remove this
		leftover, _ := io.ReadAll(reader)
		panic(fmt.Errorf("Junk remaining in buffer after lifting: %s", string(leftover)))
	}
	return item
}

func rustCallWithError[E any, U any](converter BufReader[E], callback func(*C.RustCallStatus) U) (U, E) {
	var status C.RustCallStatus
	returnValue := callback(&status)
	err := checkCallStatus(converter, status)
	return returnValue, err
}

func checkCallStatus[E any](converter BufReader[E], status C.RustCallStatus) E {
	switch status.code {
	case 0:
		var zero E
		return zero
	case 1:
		return LiftFromRustBuffer(converter, GoRustBuffer{inner: status.errorBuf})
	case 2:
		// when the rust code sees a panic, it tries to construct a rustBuffer
		// with the message.  but if that code panics, then it just sends back
		// an empty buffer.
		if status.errorBuf.len > 0 {
			panic(fmt.Errorf("%s", FfiConverterStringINSTANCE.Lift(GoRustBuffer{inner: status.errorBuf})))
		} else {
			panic(fmt.Errorf("Rust panicked while handling Rust panic"))
		}
	default:
		panic(fmt.Errorf("unknown status code: %d", status.code))
	}
}

func checkCallStatusUnknown(status C.RustCallStatus) error {
	switch status.code {
	case 0:
		return nil
	case 1:
		panic(fmt.Errorf("function not returning an error returned an error"))
	case 2:
		// when the rust code sees a panic, it tries to construct a C.RustBuffer
		// with the message.  but if that code panics, then it just sends back
		// an empty buffer.
		if status.errorBuf.len > 0 {
			panic(fmt.Errorf("%s", FfiConverterStringINSTANCE.Lift(GoRustBuffer{
				inner: status.errorBuf,
			})))
		} else {
			panic(fmt.Errorf("Rust panicked while handling Rust panic"))
		}
	default:
		return fmt.Errorf("unknown status code: %d", status.code)
	}
}

func rustCall[U any](callback func(*C.RustCallStatus) U) U {
	returnValue, err := rustCallWithError[error](nil, callback)
	if err != nil {
		panic(err)
	}
	return returnValue
}

type NativeError interface {
	AsError() error
}

func writeInt8(writer io.Writer, value int8) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeUint8(writer io.Writer, value uint8) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeInt16(writer io.Writer, value int16) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeUint16(writer io.Writer, value uint16) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeInt32(writer io.Writer, value int32) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeUint32(writer io.Writer, value uint32) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeInt64(writer io.Writer, value int64) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeUint64(writer io.Writer, value uint64) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeFloat32(writer io.Writer, value float32) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeFloat64(writer io.Writer, value float64) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func readInt8(reader io.Reader) int8 {
	var result int8
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readUint8(reader io.Reader) uint8 {
	var result uint8
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readInt16(reader io.Reader) int16 {
	var result int16
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readUint16(reader io.Reader) uint16 {
	var result uint16
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readInt32(reader io.Reader) int32 {
	var result int32
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readUint32(reader io.Reader) uint32 {
	var result uint32
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readInt64(reader io.Reader) int64 {
	var result int64
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readUint64(reader io.Reader) uint64 {
	var result uint64
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readFloat32(reader io.Reader) float32 {
	var result float32
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readFloat64(reader io.Reader) float64 {
	var result float64
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func init() {

	uniffiCheckChecksums()
}

func uniffiCheckChecksums() {
	// Get the bindings contract version from our ComponentInterface
	bindingsContractVersion := 30
	// Get the scaffolding contract version by calling the into the dylib
	scaffoldingContractVersion := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint32_t {
		return C.ffi_fauna_index_uniffi_contract_version()
	})
	if bindingsContractVersion != int(scaffoldingContractVersion) {
		// If this happens try cleaning and rebuilding your project
		panic("fauna_index: UniFFI contract version mismatch")
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_index_checksum_method_indexhandle_add_doc()
		})
		if checksum != 59650 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_index: uniffi_fauna_index_checksum_method_indexhandle_add_doc: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_index_checksum_method_indexhandle_commit()
		})
		if checksum != 4472 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_index: uniffi_fauna_index_checksum_method_indexhandle_commit: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_index_checksum_method_indexhandle_query()
		})
		if checksum != 23566 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_index: uniffi_fauna_index_checksum_method_indexhandle_query: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_index_checksum_constructor_indexhandle_create_in_ram()
		})
		if checksum != 55819 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_index: uniffi_fauna_index_checksum_constructor_indexhandle_create_in_ram: UniFFI API checksum mismatch")
		}
	}
}

type FfiConverterUint32 struct{}

var FfiConverterUint32INSTANCE = FfiConverterUint32{}

func (FfiConverterUint32) Lower(value uint32) C.uint32_t {
	return C.uint32_t(value)
}

func (FfiConverterUint32) Write(writer io.Writer, value uint32) {
	writeUint32(writer, value)
}

func (FfiConverterUint32) Lift(value C.uint32_t) uint32 {
	return uint32(value)
}

func (FfiConverterUint32) Read(reader io.Reader) uint32 {
	return readUint32(reader)
}

type FfiDestroyerUint32 struct{}

func (FfiDestroyerUint32) Destroy(_ uint32) {}

type FfiConverterInt64 struct{}

var FfiConverterInt64INSTANCE = FfiConverterInt64{}

func (FfiConverterInt64) Lower(value int64) C.int64_t {
	return C.int64_t(value)
}

func (FfiConverterInt64) Write(writer io.Writer, value int64) {
	writeInt64(writer, value)
}

func (FfiConverterInt64) Lift(value C.int64_t) int64 {
	return int64(value)
}

func (FfiConverterInt64) Read(reader io.Reader) int64 {
	return readInt64(reader)
}

type FfiDestroyerInt64 struct{}

func (FfiDestroyerInt64) Destroy(_ int64) {}

type FfiConverterFloat32 struct{}

var FfiConverterFloat32INSTANCE = FfiConverterFloat32{}

func (FfiConverterFloat32) Lower(value float32) C.float {
	return C.float(value)
}

func (FfiConverterFloat32) Write(writer io.Writer, value float32) {
	writeFloat32(writer, value)
}

func (FfiConverterFloat32) Lift(value C.float) float32 {
	return float32(value)
}

func (FfiConverterFloat32) Read(reader io.Reader) float32 {
	return readFloat32(reader)
}

type FfiDestroyerFloat32 struct{}

func (FfiDestroyerFloat32) Destroy(_ float32) {}

type FfiConverterString struct{}

var FfiConverterStringINSTANCE = FfiConverterString{}

func (FfiConverterString) Lift(rb RustBufferI) string {
	defer rb.Free()
	reader := rb.AsReader()
	b, err := io.ReadAll(reader)
	if err != nil {
		panic(fmt.Errorf("reading reader: %w", err))
	}
	return string(b)
}

func (FfiConverterString) Read(reader io.Reader) string {
	length := readInt32(reader)
	buffer := make([]byte, length)
	read_length, err := reader.Read(buffer)
	if err != nil && err != io.EOF {
		panic(err)
	}
	if read_length != int(length) {
		panic(fmt.Errorf("bad read length when reading string, expected %d, read %d", length, read_length))
	}
	return string(buffer)
}

func (FfiConverterString) Lower(value string) C.RustBuffer {
	return stringToRustBuffer(value)
}

func (c FfiConverterString) LowerExternal(value string) ExternalCRustBuffer {
	return RustBufferFromC(stringToRustBuffer(value))
}

func (FfiConverterString) Write(writer io.Writer, value string) {
	if len(value) > math.MaxInt32 {
		panic("String is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	write_length, err := io.WriteString(writer, value)
	if err != nil {
		panic(err)
	}
	if write_length != len(value) {
		panic(fmt.Errorf("bad write length when writing string, expected %d, written %d", len(value), write_length))
	}
}

type FfiDestroyerString struct{}

func (FfiDestroyerString) Destroy(_ string) {}

type FfiConverterBytes struct{}

var FfiConverterBytesINSTANCE = FfiConverterBytes{}

func (c FfiConverterBytes) Lower(value []byte) C.RustBuffer {
	return LowerIntoRustBuffer[[]byte](c, value)
}

func (c FfiConverterBytes) LowerExternal(value []byte) ExternalCRustBuffer {
	return RustBufferFromC(c.Lower(value))
}

func (c FfiConverterBytes) Write(writer io.Writer, value []byte) {
	if len(value) > math.MaxInt32 {
		panic("[]byte is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	write_length, err := writer.Write(value)
	if err != nil {
		panic(err)
	}
	if write_length != len(value) {
		panic(fmt.Errorf("bad write length when writing []byte, expected %d, written %d", len(value), write_length))
	}
}

func (c FfiConverterBytes) Lift(rb RustBufferI) []byte {
	return LiftFromRustBuffer[[]byte](c, rb)
}

func (c FfiConverterBytes) Read(reader io.Reader) []byte {
	length := readInt32(reader)
	buffer := make([]byte, length)
	read_length, err := reader.Read(buffer)
	if err != nil && err != io.EOF {
		panic(err)
	}
	if read_length != int(length) {
		panic(fmt.Errorf("bad read length when reading []byte, expected %d, read %d", length, read_length))
	}
	return buffer
}

type FfiDestroyerBytes struct{}

func (FfiDestroyerBytes) Destroy(_ []byte) {}

// Below is an implementation of synchronization requirements outlined in the link.
// https://github.com/mozilla/uniffi-rs/blob/0dc031132d9493ca812c3af6e7dd60ad2ea95bf0/uniffi_bindgen/src/bindings/kotlin/templates/ObjectRuntime.kt#L31

type FfiObject struct {
	handle        C.uint64_t
	callCounter   atomic.Int64
	cloneFunction func(C.uint64_t, *C.RustCallStatus) C.uint64_t
	freeFunction  func(C.uint64_t, *C.RustCallStatus)
	destroyed     atomic.Bool
}

func newFfiObject(
	handle C.uint64_t,
	cloneFunction func(C.uint64_t, *C.RustCallStatus) C.uint64_t,
	freeFunction func(C.uint64_t, *C.RustCallStatus),
) FfiObject {
	return FfiObject{
		handle:        handle,
		cloneFunction: cloneFunction,
		freeFunction:  freeFunction,
	}
}

func (ffiObject *FfiObject) incrementPointer(debugName string) C.uint64_t {
	for {
		counter := ffiObject.callCounter.Load()
		if counter <= -1 {
			panic(fmt.Errorf("%v object has already been destroyed", debugName))
		}
		if counter == math.MaxInt64 {
			panic(fmt.Errorf("%v object call counter would overflow", debugName))
		}
		if ffiObject.callCounter.CompareAndSwap(counter, counter+1) {
			break
		}
	}

	return rustCall(func(status *C.RustCallStatus) C.uint64_t {
		return ffiObject.cloneFunction(ffiObject.handle, status)
	})
}

func (ffiObject *FfiObject) decrementPointer() {
	if ffiObject.callCounter.Add(-1) == -1 {
		ffiObject.freeRustArcPtr()
	}
}

func (ffiObject *FfiObject) destroy() {
	if ffiObject.destroyed.CompareAndSwap(false, true) {
		if ffiObject.callCounter.Add(-1) == -1 {
			ffiObject.freeRustArcPtr()
		}
	}
}

func (ffiObject *FfiObject) freeRustArcPtr() {
	if ffiObject.handle == 0 {
		return
	}
	rustCall(func(status *C.RustCallStatus) int32 {
		ffiObject.freeFunction(ffiObject.handle, status)
		return 0
	})
}

type IndexHandleInterface interface {
	// Add one document. Caller must call `commit` before the doc is visible
	// to `query`.
	AddDoc(doc IndexedDoc) error
	// Commit pending writes. Required before `query` sees newly-added docs.
	Commit() error
	// Run a free-text query.
	//
	// `query` is parsed by Tantivy's `QueryParser` (so phrase queries with
	// `"..."` work); `kinds` filters to those `ContentKind`s; `range`
	// optionally constrains by `IndexedDoc::timestamp_ns`. `limit` caps the
	// hit count (BM25-ranked).
	//
	// Empty `query` produces zero hits (no implicit "match all" — the caller
	// must provide a non-empty query).
	Query(query string, kinds []ContentKind, varRange *TimeRange, limit uint32) ([]QueryHit, error)
}
type IndexHandle struct {
	ffiObject FfiObject
}

// Build a fresh in-memory index. Equivalent to `Index::create_in_ram()`
// but returned as an `Arc<IndexHandle>` so it can travel across the FFI
// boundary.
func IndexHandleCreateInRam() (*IndexHandle, error) {
	_uniffiRV, _uniffiErr := rustCallWithError[*IndexError](FfiConverterIndexError{}, func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_fauna_index_fn_constructor_indexhandle_create_in_ram(_uniffiStatus)
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue *IndexHandle
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterIndexHandleINSTANCE.Lift(_uniffiRV), nil
	}
}

// Add one document. Caller must call `commit` before the doc is visible
// to `query`.
func (_self *IndexHandle) AddDoc(doc IndexedDoc) error {
	_pointer := _self.ffiObject.incrementPointer("*IndexHandle")
	defer _self.ffiObject.decrementPointer()
	_, _uniffiErr := rustCallWithError[*IndexError](FfiConverterIndexError{}, func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_index_fn_method_indexhandle_add_doc(
			_pointer, FfiConverterIndexedDocINSTANCE.Lower(doc), _uniffiStatus)
		return false
	})
	return _uniffiErr.AsError()
}

// Commit pending writes. Required before `query` sees newly-added docs.
func (_self *IndexHandle) Commit() error {
	_pointer := _self.ffiObject.incrementPointer("*IndexHandle")
	defer _self.ffiObject.decrementPointer()
	_, _uniffiErr := rustCallWithError[*IndexError](FfiConverterIndexError{}, func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_index_fn_method_indexhandle_commit(
			_pointer, _uniffiStatus)
		return false
	})
	return _uniffiErr.AsError()
}

// Run a free-text query.
//
// `query` is parsed by Tantivy's `QueryParser` (so phrase queries with
// `"..."` work); `kinds` filters to those `ContentKind`s; `range`
// optionally constrains by `IndexedDoc::timestamp_ns`. `limit` caps the
// hit count (BM25-ranked).
//
// Empty `query` produces zero hits (no implicit "match all" — the caller
// must provide a non-empty query).
func (_self *IndexHandle) Query(query string, kinds []ContentKind, varRange *TimeRange, limit uint32) ([]QueryHit, error) {
	_pointer := _self.ffiObject.incrementPointer("*IndexHandle")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*IndexError](FfiConverterIndexError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_index_fn_method_indexhandle_query(
				_pointer, FfiConverterStringINSTANCE.Lower(query), FfiConverterSequenceContentKindINSTANCE.Lower(kinds), FfiConverterOptionalTimeRangeINSTANCE.Lower(varRange), FfiConverterUint32INSTANCE.Lower(limit), _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue []QueryHit
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterSequenceQueryHitINSTANCE.Lift(_uniffiRV), nil
	}
}
func (object *IndexHandle) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterIndexHandle struct{}

var FfiConverterIndexHandleINSTANCE = FfiConverterIndexHandle{}

func (c FfiConverterIndexHandle) Lift(handle C.uint64_t) *IndexHandle {
	result := &IndexHandle{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_fauna_index_fn_clone_indexhandle(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_fauna_index_fn_free_indexhandle(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*IndexHandle).Destroy)
	return result
}

func (c FfiConverterIndexHandle) Read(reader io.Reader) *IndexHandle {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterIndexHandle) Lower(value *IndexHandle) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*IndexHandle")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterIndexHandle) Write(writer io.Writer, value *IndexHandle) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalIndexHandle(handle uint64) *IndexHandle {
	return FfiConverterIndexHandleINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalIndexHandle(value *IndexHandle) uint64 {
	return uint64(FfiConverterIndexHandleINSTANCE.Lower(value))
}

type FfiDestroyerIndexHandle struct{}

func (_ FfiDestroyerIndexHandle) Destroy(value *IndexHandle) {
	value.Destroy()
}

// One document being added to the index.
type IndexedDoc struct {
	Kind      ContentKind
	ContentId ContentId
	// Unix nanos. Used for time-range filtering and as a tie-breaker on rank.
	TimestampNs int64
	// Optional sender / author. Only populated for kinds that have one.
	SenderActorId *[]byte
	// Optional *secondary* identity — a second producer-owned spelling of the
	// same document, stored (never searched) so a caller that knows a doc only
	// by this spelling can decide coverage without touching the content id.
	//
	// Exactly one consumer today (`content-index.md` § Where the index is
	// built → the 2026-08-10 carrier ruling): mail docs carry the raw 32-byte
	// nest message id here, which is the only identity the MDA's IMAP `SEARCH`
	// path holds, while `content_id` stays the ratified RFC `Message-ID`.
	// It is a lookup carrier, not a substitute key — `add_doc`'s upsert and
	// the stage-time guards key on `content_id` alone.
	SecondaryId *[]byte
	Fields      []IndexedField
}

func (r *IndexedDoc) Destroy() {
	FfiDestroyerContentKind{}.Destroy(r.Kind)
	FfiDestroyerTypeContentId{}.Destroy(r.ContentId)
	FfiDestroyerInt64{}.Destroy(r.TimestampNs)
	FfiDestroyerOptionalBytes{}.Destroy(r.SenderActorId)
	FfiDestroyerOptionalBytes{}.Destroy(r.SecondaryId)
	FfiDestroyerSequenceIndexedField{}.Destroy(r.Fields)
}

type FfiConverterIndexedDoc struct{}

var FfiConverterIndexedDocINSTANCE = FfiConverterIndexedDoc{}

func (c FfiConverterIndexedDoc) Lift(rb RustBufferI) IndexedDoc {
	return LiftFromRustBuffer[IndexedDoc](c, rb)
}

func (c FfiConverterIndexedDoc) Read(reader io.Reader) IndexedDoc {
	return IndexedDoc{
		FfiConverterContentKindINSTANCE.Read(reader),
		FfiConverterTypeContentIdINSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterOptionalBytesINSTANCE.Read(reader),
		FfiConverterOptionalBytesINSTANCE.Read(reader),
		FfiConverterSequenceIndexedFieldINSTANCE.Read(reader),
	}
}

func (c FfiConverterIndexedDoc) Lower(value IndexedDoc) C.RustBuffer {
	return LowerIntoRustBuffer[IndexedDoc](c, value)
}

func (c FfiConverterIndexedDoc) LowerExternal(value IndexedDoc) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[IndexedDoc](c, value))
}

func (c FfiConverterIndexedDoc) Write(writer io.Writer, value IndexedDoc) {
	FfiConverterContentKindINSTANCE.Write(writer, value.Kind)
	FfiConverterTypeContentIdINSTANCE.Write(writer, value.ContentId)
	FfiConverterInt64INSTANCE.Write(writer, value.TimestampNs)
	FfiConverterOptionalBytesINSTANCE.Write(writer, value.SenderActorId)
	FfiConverterOptionalBytesINSTANCE.Write(writer, value.SecondaryId)
	FfiConverterSequenceIndexedFieldINSTANCE.Write(writer, value.Fields)
}

type FfiDestroyerIndexedDoc struct{}

func (_ FfiDestroyerIndexedDoc) Destroy(value IndexedDoc) {
	value.Destroy()
}

type IndexedField struct {
	Kind FieldKind
	// For Title and Body: raw text. For PreTokenizedTags: a vec of
	// already-canonical tokens encoded as a single space-separated string.
	Text string
}

func (r *IndexedField) Destroy() {
	FfiDestroyerFieldKind{}.Destroy(r.Kind)
	FfiDestroyerString{}.Destroy(r.Text)
}

type FfiConverterIndexedField struct{}

var FfiConverterIndexedFieldINSTANCE = FfiConverterIndexedField{}

func (c FfiConverterIndexedField) Lift(rb RustBufferI) IndexedField {
	return LiftFromRustBuffer[IndexedField](c, rb)
}

func (c FfiConverterIndexedField) Read(reader io.Reader) IndexedField {
	return IndexedField{
		FfiConverterFieldKindINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterIndexedField) Lower(value IndexedField) C.RustBuffer {
	return LowerIntoRustBuffer[IndexedField](c, value)
}

func (c FfiConverterIndexedField) LowerExternal(value IndexedField) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[IndexedField](c, value))
}

func (c FfiConverterIndexedField) Write(writer io.Writer, value IndexedField) {
	FfiConverterFieldKindINSTANCE.Write(writer, value.Kind)
	FfiConverterStringINSTANCE.Write(writer, value.Text)
}

type FfiDestroyerIndexedField struct{}

func (_ FfiDestroyerIndexedField) Destroy(value IndexedField) {
	value.Destroy()
}

// One result from `Index::query`.
type QueryHit struct {
	Kind          ContentKind
	ContentId     ContentId
	TimestampNs   int64
	SenderActorId *[]byte
	// BM25 score from Tantivy; higher is more relevant.
	Score float32
}

func (r *QueryHit) Destroy() {
	FfiDestroyerContentKind{}.Destroy(r.Kind)
	FfiDestroyerTypeContentId{}.Destroy(r.ContentId)
	FfiDestroyerInt64{}.Destroy(r.TimestampNs)
	FfiDestroyerOptionalBytes{}.Destroy(r.SenderActorId)
	FfiDestroyerFloat32{}.Destroy(r.Score)
}

type FfiConverterQueryHit struct{}

var FfiConverterQueryHitINSTANCE = FfiConverterQueryHit{}

func (c FfiConverterQueryHit) Lift(rb RustBufferI) QueryHit {
	return LiftFromRustBuffer[QueryHit](c, rb)
}

func (c FfiConverterQueryHit) Read(reader io.Reader) QueryHit {
	return QueryHit{
		FfiConverterContentKindINSTANCE.Read(reader),
		FfiConverterTypeContentIdINSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterOptionalBytesINSTANCE.Read(reader),
		FfiConverterFloat32INSTANCE.Read(reader),
	}
}

func (c FfiConverterQueryHit) Lower(value QueryHit) C.RustBuffer {
	return LowerIntoRustBuffer[QueryHit](c, value)
}

func (c FfiConverterQueryHit) LowerExternal(value QueryHit) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[QueryHit](c, value))
}

func (c FfiConverterQueryHit) Write(writer io.Writer, value QueryHit) {
	FfiConverterContentKindINSTANCE.Write(writer, value.Kind)
	FfiConverterTypeContentIdINSTANCE.Write(writer, value.ContentId)
	FfiConverterInt64INSTANCE.Write(writer, value.TimestampNs)
	FfiConverterOptionalBytesINSTANCE.Write(writer, value.SenderActorId)
	FfiConverterFloat32INSTANCE.Write(writer, value.Score)
}

type FfiDestroyerQueryHit struct{}

func (_ FfiDestroyerQueryHit) Destroy(value QueryHit) {
	value.Destroy()
}

// Half-open `[start_ns, end_ns)` filter on `IndexedDoc::timestamp_ns`.
type TimeRange struct {
	StartNs int64
	EndNs   int64
}

func (r *TimeRange) Destroy() {
	FfiDestroyerInt64{}.Destroy(r.StartNs)
	FfiDestroyerInt64{}.Destroy(r.EndNs)
}

type FfiConverterTimeRange struct{}

var FfiConverterTimeRangeINSTANCE = FfiConverterTimeRange{}

func (c FfiConverterTimeRange) Lift(rb RustBufferI) TimeRange {
	return LiftFromRustBuffer[TimeRange](c, rb)
}

func (c FfiConverterTimeRange) Read(reader io.Reader) TimeRange {
	return TimeRange{
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
	}
}

func (c FfiConverterTimeRange) Lower(value TimeRange) C.RustBuffer {
	return LowerIntoRustBuffer[TimeRange](c, value)
}

func (c FfiConverterTimeRange) LowerExternal(value TimeRange) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[TimeRange](c, value))
}

func (c FfiConverterTimeRange) Write(writer io.Writer, value TimeRange) {
	FfiConverterInt64INSTANCE.Write(writer, value.StartNs)
	FfiConverterInt64INSTANCE.Write(writer, value.EndNs)
}

type FfiDestroyerTimeRange struct{}

func (_ FfiDestroyerTimeRange) Destroy(value TimeRange) {
	value.Destroy()
}

// One of the eight content kinds tracked per the encryption-at-rest target
// doc. The variant order is part of the wire format and must not be reordered.
//
// `Ord` is derived, so it follows that same declaration order — which is what
// gives a multi-kind builder a deterministic publish order for the several
// segments one flush can produce. It is an ordering of convenience, not of
// meaning: no kind ranks above another.
//
// **Closed by design** (`transport.md` § Schema and forward-compat discipline
// → *Rule 3 in full*, the `ladder` ground: a new variant raises the format
// version the reader checks before it decodes, so there is no unknown arm). A
// new variant is an edit to `tools/check-additive-evolution/enum_ledger.txt`,
// made in the same change.
type ContentKind uint

const (
	ContentKindMail         ContentKind = 1
	ContentKindCalendar     ContentKind = 2
	ContentKindConversation ContentKind = 3
	ContentKindPost         ContentKind = 4
	ContentKindFile         ContentKind = 5
	ContentKindContact      ContentKind = 6
	ContentKindDraft        ContentKind = 7
	ContentKindMedia        ContentKind = 8
)

type FfiConverterContentKind struct{}

var FfiConverterContentKindINSTANCE = FfiConverterContentKind{}

func (c FfiConverterContentKind) Lift(rb RustBufferI) ContentKind {
	return LiftFromRustBuffer[ContentKind](c, rb)
}

func (c FfiConverterContentKind) Lower(value ContentKind) C.RustBuffer {
	return LowerIntoRustBuffer[ContentKind](c, value)
}

func (c FfiConverterContentKind) LowerExternal(value ContentKind) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ContentKind](c, value))
}
func (FfiConverterContentKind) Read(reader io.Reader) ContentKind {
	id := readInt32(reader)
	return ContentKind(id)
}

func (FfiConverterContentKind) Write(writer io.Writer, value ContentKind) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerContentKind struct{}

func (_ FfiDestroyerContentKind) Destroy(value ContentKind) {
}

// Which searchable surface of a doc a field represents.
type FieldKind uint

const (
	// Short, high-signal field: subject, title, contact name, file name.
	FieldKindTitle FieldKind = 1
	// Free-form long text: mail body, post body, file contents, transcript.
	FieldKindBody FieldKind = 2
	// Pre-tokenized tag tokens (intended for classifier output, e.g.
	// `tag-dog tag-beach` from a moderate-scope image classifier per spec
	// D7). Currently tokenized identically to body text — the
	// `FieldKind::PreTokenizedTags` enum variant exists to mark caller
	// intent and to give the schema room to grow a non-tokenizing
	// pipeline later (Plan 7). Until then, choose tag formats that
	// survive UAX#29 word segmentation (avoid `:` and other punctuation
	// that splits the token).
	FieldKindPreTokenizedTags FieldKind = 3
)

type FfiConverterFieldKind struct{}

var FfiConverterFieldKindINSTANCE = FfiConverterFieldKind{}

func (c FfiConverterFieldKind) Lift(rb RustBufferI) FieldKind {
	return LiftFromRustBuffer[FieldKind](c, rb)
}

func (c FfiConverterFieldKind) Lower(value FieldKind) C.RustBuffer {
	return LowerIntoRustBuffer[FieldKind](c, value)
}

func (c FfiConverterFieldKind) LowerExternal(value FieldKind) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[FieldKind](c, value))
}
func (FfiConverterFieldKind) Read(reader io.Reader) FieldKind {
	id := readInt32(reader)
	return FieldKind(id)
}

func (FfiConverterFieldKind) Write(writer io.Writer, value FieldKind) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerFieldKind struct{}

func (_ FfiDestroyerFieldKind) Destroy(value FieldKind) {
}

type IndexError struct {
	err error
}

// Convenience method to turn *IndexError into error
// Avoiding treating nil pointer as non nil error interface
func (err *IndexError) AsError() error {
	if err == nil {
		return nil
	} else {
		return err
	}
}

func (err IndexError) Error() string {
	return fmt.Sprintf("IndexError: %s", err.err.Error())
}

func (err IndexError) Unwrap() error {
	return err.err
}

// Err* are used for checking error type with `errors.Is`
var ErrIndexErrorTantivy = fmt.Errorf("IndexErrorTantivy")
var ErrIndexErrorQueryParse = fmt.Errorf("IndexErrorQueryParse")
var ErrIndexErrorInvalidQuery = fmt.Errorf("IndexErrorInvalidQuery")
var ErrIndexErrorIo = fmt.Errorf("IndexErrorIo")
var ErrIndexErrorSchemaMismatch = fmt.Errorf("IndexErrorSchemaMismatch")
var ErrIndexErrorCrypto = fmt.Errorf("IndexErrorCrypto")
var ErrIndexErrorIncompatible = fmt.Errorf("IndexErrorIncompatible")
var ErrIndexErrorWrongKindClass = fmt.Errorf("IndexErrorWrongKindClass")

// Variant structs
type IndexErrorTantivy struct {
	message string
}

func NewIndexErrorTantivy() *IndexError {
	return &IndexError{err: &IndexErrorTantivy{}}
}

func (e IndexErrorTantivy) destroy() {
}

func (err IndexErrorTantivy) Error() string {
	return fmt.Sprintf("Tantivy: %s", err.message)
}

func (self IndexErrorTantivy) Is(target error) bool {
	return target == ErrIndexErrorTantivy
}

type IndexErrorQueryParse struct {
	message string
}

func NewIndexErrorQueryParse() *IndexError {
	return &IndexError{err: &IndexErrorQueryParse{}}
}

func (e IndexErrorQueryParse) destroy() {
}

func (err IndexErrorQueryParse) Error() string {
	return fmt.Sprintf("QueryParse: %s", err.message)
}

func (self IndexErrorQueryParse) Is(target error) bool {
	return target == ErrIndexErrorQueryParse
}

type IndexErrorInvalidQuery struct {
	message string
}

func NewIndexErrorInvalidQuery() *IndexError {
	return &IndexError{err: &IndexErrorInvalidQuery{}}
}

func (e IndexErrorInvalidQuery) destroy() {
}

func (err IndexErrorInvalidQuery) Error() string {
	return fmt.Sprintf("InvalidQuery: %s", err.message)
}

func (self IndexErrorInvalidQuery) Is(target error) bool {
	return target == ErrIndexErrorInvalidQuery
}

type IndexErrorIo struct {
	message string
}

func NewIndexErrorIo() *IndexError {
	return &IndexError{err: &IndexErrorIo{}}
}

func (e IndexErrorIo) destroy() {
}

func (err IndexErrorIo) Error() string {
	return fmt.Sprintf("Io: %s", err.message)
}

func (self IndexErrorIo) Is(target error) bool {
	return target == ErrIndexErrorIo
}

type IndexErrorSchemaMismatch struct {
	message string
}

func NewIndexErrorSchemaMismatch() *IndexError {
	return &IndexError{err: &IndexErrorSchemaMismatch{}}
}

func (e IndexErrorSchemaMismatch) destroy() {
}

func (err IndexErrorSchemaMismatch) Error() string {
	return fmt.Sprintf("SchemaMismatch: %s", err.message)
}

func (self IndexErrorSchemaMismatch) Is(target error) bool {
	return target == ErrIndexErrorSchemaMismatch
}

type IndexErrorCrypto struct {
	message string
}

func NewIndexErrorCrypto() *IndexError {
	return &IndexError{err: &IndexErrorCrypto{}}
}

func (e IndexErrorCrypto) destroy() {
}

func (err IndexErrorCrypto) Error() string {
	return fmt.Sprintf("Crypto: %s", err.message)
}

func (self IndexErrorCrypto) Is(target error) bool {
	return target == ErrIndexErrorCrypto
}

// The blob was written by a **newer** build that raised the reader floor past this
// one: it is intact, and this binary must not touch it.
//
// Deliberately distinct from [`IndexError::Crypto`] and
// [`IndexError::SchemaMismatch`]. Collapsing "written by a newer build — update the
// app, the data is fine" into a generic "corrupt" is what licenses a caller to
// *heal* the blob by overwriting it, and that conflation is exactly what produced
// the account-index cliff (`version-compatibility.md` § 5 item 9).
// A caller matching this variant can say the true thing and leave the bytes alone.
type IndexErrorIncompatible struct {
	message string
}

// The blob was written by a **newer** build that raised the reader floor past this
// one: it is intact, and this binary must not touch it.
//
// Deliberately distinct from [`IndexError::Crypto`] and
// [`IndexError::SchemaMismatch`]. Collapsing "written by a newer build — update the
// app, the data is fine" into a generic "corrupt" is what licenses a caller to
// *heal* the blob by overwriting it, and that conflation is exactly what produced
// the account-index cliff (`version-compatibility.md` § 5 item 9).
// A caller matching this variant can say the true thing and leave the bytes alone.
func NewIndexErrorIncompatible() *IndexError {
	return &IndexError{err: &IndexErrorIncompatible{}}
}

func (e IndexErrorIncompatible) destroy() {
}

func (err IndexErrorIncompatible) Error() string {
	return fmt.Sprintf("Incompatible: %s", err.message)
}

func (self IndexErrorIncompatible) Is(target error) bool {
	return target == ErrIndexErrorIncompatible
}

// A kind was offered to the wrong-class manifest (a mail/calendar kind to
// `manifest.idx`, or a master-class kind to `manifest-mailcal.idx`). The
// split is refused at the API, never trusted to convention
// (`content-index.md` § Encryption posture).
type IndexErrorWrongKindClass struct {
	message string
}

// A kind was offered to the wrong-class manifest (a mail/calendar kind to
// `manifest.idx`, or a master-class kind to `manifest-mailcal.idx`). The
// split is refused at the API, never trusted to convention
// (`content-index.md` § Encryption posture).
func NewIndexErrorWrongKindClass() *IndexError {
	return &IndexError{err: &IndexErrorWrongKindClass{}}
}

func (e IndexErrorWrongKindClass) destroy() {
}

func (err IndexErrorWrongKindClass) Error() string {
	return fmt.Sprintf("WrongKindClass: %s", err.message)
}

func (self IndexErrorWrongKindClass) Is(target error) bool {
	return target == ErrIndexErrorWrongKindClass
}

type FfiConverterIndexError struct{}

var FfiConverterIndexErrorINSTANCE = FfiConverterIndexError{}

func (c FfiConverterIndexError) Lift(eb RustBufferI) *IndexError {
	return LiftFromRustBuffer[*IndexError](c, eb)
}

func (c FfiConverterIndexError) Lower(value *IndexError) C.RustBuffer {
	return LowerIntoRustBuffer[*IndexError](c, value)
}

func (c FfiConverterIndexError) LowerExternal(value *IndexError) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*IndexError](c, value))
}

func (c FfiConverterIndexError) Read(reader io.Reader) *IndexError {
	errorID := readUint32(reader)

	message := FfiConverterStringINSTANCE.Read(reader)
	switch errorID {
	case 1:
		return &IndexError{&IndexErrorTantivy{message}}
	case 2:
		return &IndexError{&IndexErrorQueryParse{message}}
	case 3:
		return &IndexError{&IndexErrorInvalidQuery{message}}
	case 4:
		return &IndexError{&IndexErrorIo{message}}
	case 5:
		return &IndexError{&IndexErrorSchemaMismatch{message}}
	case 6:
		return &IndexError{&IndexErrorCrypto{message}}
	case 7:
		return &IndexError{&IndexErrorIncompatible{message}}
	case 8:
		return &IndexError{&IndexErrorWrongKindClass{message}}
	default:
		panic(fmt.Sprintf("Unknown error code %d in FfiConverterIndexError.Read()", errorID))
	}

}

func (c FfiConverterIndexError) Write(writer io.Writer, value *IndexError) {
	switch variantValue := value.err.(type) {
	case *IndexErrorTantivy:
		writeInt32(writer, 1)
	case *IndexErrorQueryParse:
		writeInt32(writer, 2)
	case *IndexErrorInvalidQuery:
		writeInt32(writer, 3)
	case *IndexErrorIo:
		writeInt32(writer, 4)
	case *IndexErrorSchemaMismatch:
		writeInt32(writer, 5)
	case *IndexErrorCrypto:
		writeInt32(writer, 6)
	case *IndexErrorIncompatible:
		writeInt32(writer, 7)
	case *IndexErrorWrongKindClass:
		writeInt32(writer, 8)
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiConverterIndexError.Write", value))
	}
}

type FfiDestroyerIndexError struct{}

func (_ FfiDestroyerIndexError) Destroy(value *IndexError) {
	switch variantValue := value.err.(type) {
	case IndexErrorTantivy:
		variantValue.destroy()
	case IndexErrorQueryParse:
		variantValue.destroy()
	case IndexErrorInvalidQuery:
		variantValue.destroy()
	case IndexErrorIo:
		variantValue.destroy()
	case IndexErrorSchemaMismatch:
		variantValue.destroy()
	case IndexErrorCrypto:
		variantValue.destroy()
	case IndexErrorIncompatible:
		variantValue.destroy()
	case IndexErrorWrongKindClass:
		variantValue.destroy()
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiDestroyerIndexError.Destroy", value))
	}
}

// The two key classes of the per-kind split: `MailCal` kinds (mail, calendar)
// wrap under the MSEK-derived [`crate::IndexSegmentKey`] and live in
// `manifest-mailcal.idx`; `Master` kinds wrap under the [`crate::IndexMasterKey`]
// and live in `manifest.idx`. One manifest file per class; a kind never appears
// in the wrong-class manifest ([`IndexError::WrongKindClass`]).
//
// **Closed by design** (`transport.md` § Schema and forward-compat discipline
// → *Rule 3 in full*, the `ladder` ground: a new variant raises the format
// version the reader checks before it decodes, so there is no unknown arm). A
// new variant is an edit to `tools/check-additive-evolution/enum_ledger.txt`,
// made in the same change.
type KindClass uint

const (
	KindClassMailCal KindClass = 1
	KindClassMaster  KindClass = 2
)

type FfiConverterKindClass struct{}

var FfiConverterKindClassINSTANCE = FfiConverterKindClass{}

func (c FfiConverterKindClass) Lift(rb RustBufferI) KindClass {
	return LiftFromRustBuffer[KindClass](c, rb)
}

func (c FfiConverterKindClass) Lower(value KindClass) C.RustBuffer {
	return LowerIntoRustBuffer[KindClass](c, value)
}

func (c FfiConverterKindClass) LowerExternal(value KindClass) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[KindClass](c, value))
}
func (FfiConverterKindClass) Read(reader io.Reader) KindClass {
	id := readInt32(reader)
	return KindClass(id)
}

func (FfiConverterKindClass) Write(writer io.Writer, value KindClass) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerKindClass struct{}

func (_ FfiDestroyerKindClass) Destroy(value KindClass) {
}

type FfiConverterOptionalBytes struct{}

var FfiConverterOptionalBytesINSTANCE = FfiConverterOptionalBytes{}

func (c FfiConverterOptionalBytes) Lift(rb RustBufferI) *[]byte {
	return LiftFromRustBuffer[*[]byte](c, rb)
}

func (_ FfiConverterOptionalBytes) Read(reader io.Reader) *[]byte {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterBytesINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalBytes) Lower(value *[]byte) C.RustBuffer {
	return LowerIntoRustBuffer[*[]byte](c, value)
}

func (c FfiConverterOptionalBytes) LowerExternal(value *[]byte) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*[]byte](c, value))
}

func (_ FfiConverterOptionalBytes) Write(writer io.Writer, value *[]byte) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterBytesINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalBytes struct{}

func (_ FfiDestroyerOptionalBytes) Destroy(value *[]byte) {
	if value != nil {
		FfiDestroyerBytes{}.Destroy(*value)
	}
}

type FfiConverterOptionalTimeRange struct{}

var FfiConverterOptionalTimeRangeINSTANCE = FfiConverterOptionalTimeRange{}

func (c FfiConverterOptionalTimeRange) Lift(rb RustBufferI) *TimeRange {
	return LiftFromRustBuffer[*TimeRange](c, rb)
}

func (_ FfiConverterOptionalTimeRange) Read(reader io.Reader) *TimeRange {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterTimeRangeINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalTimeRange) Lower(value *TimeRange) C.RustBuffer {
	return LowerIntoRustBuffer[*TimeRange](c, value)
}

func (c FfiConverterOptionalTimeRange) LowerExternal(value *TimeRange) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*TimeRange](c, value))
}

func (_ FfiConverterOptionalTimeRange) Write(writer io.Writer, value *TimeRange) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterTimeRangeINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalTimeRange struct{}

func (_ FfiDestroyerOptionalTimeRange) Destroy(value *TimeRange) {
	if value != nil {
		FfiDestroyerTimeRange{}.Destroy(*value)
	}
}

type FfiConverterSequenceIndexedField struct{}

var FfiConverterSequenceIndexedFieldINSTANCE = FfiConverterSequenceIndexedField{}

func (c FfiConverterSequenceIndexedField) Lift(rb RustBufferI) []IndexedField {
	return LiftFromRustBuffer[[]IndexedField](c, rb)
}

func (c FfiConverterSequenceIndexedField) Read(reader io.Reader) []IndexedField {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]IndexedField, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterIndexedFieldINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceIndexedField) Lower(value []IndexedField) C.RustBuffer {
	return LowerIntoRustBuffer[[]IndexedField](c, value)
}

func (c FfiConverterSequenceIndexedField) LowerExternal(value []IndexedField) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]IndexedField](c, value))
}

func (c FfiConverterSequenceIndexedField) Write(writer io.Writer, value []IndexedField) {
	if len(value) > math.MaxInt32 {
		panic("[]IndexedField is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterIndexedFieldINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceIndexedField struct{}

func (FfiDestroyerSequenceIndexedField) Destroy(sequence []IndexedField) {
	for _, value := range sequence {
		FfiDestroyerIndexedField{}.Destroy(value)
	}
}

type FfiConverterSequenceQueryHit struct{}

var FfiConverterSequenceQueryHitINSTANCE = FfiConverterSequenceQueryHit{}

func (c FfiConverterSequenceQueryHit) Lift(rb RustBufferI) []QueryHit {
	return LiftFromRustBuffer[[]QueryHit](c, rb)
}

func (c FfiConverterSequenceQueryHit) Read(reader io.Reader) []QueryHit {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]QueryHit, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterQueryHitINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceQueryHit) Lower(value []QueryHit) C.RustBuffer {
	return LowerIntoRustBuffer[[]QueryHit](c, value)
}

func (c FfiConverterSequenceQueryHit) LowerExternal(value []QueryHit) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]QueryHit](c, value))
}

func (c FfiConverterSequenceQueryHit) Write(writer io.Writer, value []QueryHit) {
	if len(value) > math.MaxInt32 {
		panic("[]QueryHit is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterQueryHitINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceQueryHit struct{}

func (FfiDestroyerSequenceQueryHit) Destroy(sequence []QueryHit) {
	for _, value := range sequence {
		FfiDestroyerQueryHit{}.Destroy(value)
	}
}

type FfiConverterSequenceContentKind struct{}

var FfiConverterSequenceContentKindINSTANCE = FfiConverterSequenceContentKind{}

func (c FfiConverterSequenceContentKind) Lift(rb RustBufferI) []ContentKind {
	return LiftFromRustBuffer[[]ContentKind](c, rb)
}

func (c FfiConverterSequenceContentKind) Read(reader io.Reader) []ContentKind {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ContentKind, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterContentKindINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceContentKind) Lower(value []ContentKind) C.RustBuffer {
	return LowerIntoRustBuffer[[]ContentKind](c, value)
}

func (c FfiConverterSequenceContentKind) LowerExternal(value []ContentKind) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ContentKind](c, value))
}

func (c FfiConverterSequenceContentKind) Write(writer io.Writer, value []ContentKind) {
	if len(value) > math.MaxInt32 {
		panic("[]ContentKind is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterContentKindINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceContentKind struct{}

func (FfiDestroyerSequenceContentKind) Destroy(sequence []ContentKind) {
	for _, value := range sequence {
		FfiDestroyerContentKind{}.Destroy(value)
	}
}

/**
 * Typealias from the type name used in the UDL file to the builtin type.  This
 * is needed because the UDL type name is used in function/method signatures.
 * It's also what we have an external type that references a custom type.
 */
type ContentId = []byte
type FfiConverterTypeContentId = FfiConverterBytes
type FfiDestroyerTypeContentId = FfiDestroyerBytes

var FfiConverterTypeContentIdINSTANCE = FfiConverterBytes{}

func LiftFromExternalTypeContentId(value ExternalCRustBuffer) ContentId {
	return FfiConverterTypeContentIdINSTANCE.Lift(RustBufferFromExternal(value))
}

func LowerToExternalTypeContentId(value ContentId) ExternalCRustBuffer {
	return RustBufferFromC(FfiConverterTypeContentIdINSTANCE.Lower(value))
}
