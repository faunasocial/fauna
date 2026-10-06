package fauna_client_moderation

// #include <fauna_client_moderation.h>
import "C"

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"io"
	"math"
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
		C.ffi_fauna_client_moderation_rustbuffer_free(cb.inner, status)
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
		return C.ffi_fauna_client_moderation_rustbuffer_from_bytes(foreign, status)
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
		return C.ffi_fauna_client_moderation_uniffi_contract_version()
	})
	if bindingsContractVersion != int(scaffoldingContractVersion) {
		// If this happens try cleaning and rebuilding your project
		panic("fauna_client_moderation: UniFFI contract version mismatch")
	}
}

type FfiConverterUint8 struct{}

var FfiConverterUint8INSTANCE = FfiConverterUint8{}

func (FfiConverterUint8) Lower(value uint8) C.uint8_t {
	return C.uint8_t(value)
}

func (FfiConverterUint8) Write(writer io.Writer, value uint8) {
	writeUint8(writer, value)
}

func (FfiConverterUint8) Lift(value C.uint8_t) uint8 {
	return uint8(value)
}

func (FfiConverterUint8) Read(reader io.Reader) uint8 {
	return readUint8(reader)
}

type FfiDestroyerUint8 struct{}

func (FfiDestroyerUint8) Destroy(_ uint8) {}

type FfiConverterUint16 struct{}

var FfiConverterUint16INSTANCE = FfiConverterUint16{}

func (FfiConverterUint16) Lower(value uint16) C.uint16_t {
	return C.uint16_t(value)
}

func (FfiConverterUint16) Write(writer io.Writer, value uint16) {
	writeUint16(writer, value)
}

func (FfiConverterUint16) Lift(value C.uint16_t) uint16 {
	return uint16(value)
}

func (FfiConverterUint16) Read(reader io.Reader) uint16 {
	return readUint16(reader)
}

type FfiDestroyerUint16 struct{}

func (FfiDestroyerUint16) Destroy(_ uint16) {}

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

// A retained client-side post-decrypt detection — the encrypted-mode
// social-content moderation signal. Carries **no** enforcement `action` (that is
// the server queue's half); its queue row shows a blank action column.
type LocalDetection struct {
	// The classified content's id (a conversation channel / post id — the caller's
	// content ref, the same key [`ObligationAction::content_id`] uses so the two
	// sources dedupe).
	ContentId string
	// The content kind (`"post"`, `"message"`, …) for the row's content ref; may
	// be empty when the hook has no kind to attribute.
	ContentType string
	// The detected category — [`LOCAL_DETECTION_CATEGORY`] today.
	Category string
	// Confidence per-mille (0–1000), matching [`ObligationAction::confidence_per_mille`].
	ConfidencePerMille uint16
	// Detection time, same unit as [`ObligationAction::timestamp`] (microsecond
	// epoch), supplied by the caller.
	Timestamp int64
}

func (r *LocalDetection) Destroy() {
	FfiDestroyerString{}.Destroy(r.ContentId)
	FfiDestroyerString{}.Destroy(r.ContentType)
	FfiDestroyerString{}.Destroy(r.Category)
	FfiDestroyerUint16{}.Destroy(r.ConfidencePerMille)
	FfiDestroyerInt64{}.Destroy(r.Timestamp)
}

type FfiConverterLocalDetection struct{}

var FfiConverterLocalDetectionINSTANCE = FfiConverterLocalDetection{}

func (c FfiConverterLocalDetection) Lift(rb RustBufferI) LocalDetection {
	return LiftFromRustBuffer[LocalDetection](c, rb)
}

func (c FfiConverterLocalDetection) Read(reader io.Reader) LocalDetection {
	return LocalDetection{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUint16INSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
	}
}

func (c FfiConverterLocalDetection) Lower(value LocalDetection) C.RustBuffer {
	return LowerIntoRustBuffer[LocalDetection](c, value)
}

func (c FfiConverterLocalDetection) LowerExternal(value LocalDetection) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[LocalDetection](c, value))
}

func (c FfiConverterLocalDetection) Write(writer io.Writer, value LocalDetection) {
	FfiConverterStringINSTANCE.Write(writer, value.ContentId)
	FfiConverterStringINSTANCE.Write(writer, value.ContentType)
	FfiConverterStringINSTANCE.Write(writer, value.Category)
	FfiConverterUint16INSTANCE.Write(writer, value.ConfidencePerMille)
	FfiConverterInt64INSTANCE.Write(writer, value.Timestamp)
}

type FfiDestroyerLocalDetection struct{}

func (_ FfiDestroyerLocalDetection) Destroy(value LocalDetection) {
	value.Destroy()
}

// One unified moderation-queue row — the merge of the two sources. Uniform across
// server and local rows so a client's row renderer branches only on `action`
// being `Some`/`None` (never on which list a row came from).
type QueueRow struct {
	ContentId          string
	ContentType        string
	Category           string
	ConfidencePerMille uint16
	// The enforcement action discriminant for a server row; **`None`** for a local
	// detection (blank action column — never fabricate one, `moderation.md` § Don't
	// do these).
	Action    *uint8
	Timestamp int64
	Source    QueueRowSource
}

func (r *QueueRow) Destroy() {
	FfiDestroyerString{}.Destroy(r.ContentId)
	FfiDestroyerString{}.Destroy(r.ContentType)
	FfiDestroyerString{}.Destroy(r.Category)
	FfiDestroyerUint16{}.Destroy(r.ConfidencePerMille)
	FfiDestroyerOptionalUint8{}.Destroy(r.Action)
	FfiDestroyerInt64{}.Destroy(r.Timestamp)
	FfiDestroyerQueueRowSource{}.Destroy(r.Source)
}

type FfiConverterQueueRow struct{}

var FfiConverterQueueRowINSTANCE = FfiConverterQueueRow{}

func (c FfiConverterQueueRow) Lift(rb RustBufferI) QueueRow {
	return LiftFromRustBuffer[QueueRow](c, rb)
}

func (c FfiConverterQueueRow) Read(reader io.Reader) QueueRow {
	return QueueRow{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUint16INSTANCE.Read(reader),
		FfiConverterOptionalUint8INSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterQueueRowSourceINSTANCE.Read(reader),
	}
}

func (c FfiConverterQueueRow) Lower(value QueueRow) C.RustBuffer {
	return LowerIntoRustBuffer[QueueRow](c, value)
}

func (c FfiConverterQueueRow) LowerExternal(value QueueRow) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[QueueRow](c, value))
}

func (c FfiConverterQueueRow) Write(writer io.Writer, value QueueRow) {
	FfiConverterStringINSTANCE.Write(writer, value.ContentId)
	FfiConverterStringINSTANCE.Write(writer, value.ContentType)
	FfiConverterStringINSTANCE.Write(writer, value.Category)
	FfiConverterUint16INSTANCE.Write(writer, value.ConfidencePerMille)
	FfiConverterOptionalUint8INSTANCE.Write(writer, value.Action)
	FfiConverterInt64INSTANCE.Write(writer, value.Timestamp)
	FfiConverterQueueRowSourceINSTANCE.Write(writer, value.Source)
}

type FfiDestroyerQueueRow struct{}

func (_ FfiDestroyerQueueRow) Destroy(value QueueRow) {
	value.Destroy()
}

// Which source a [`QueueRow`] came from — the server obligation queue or a local
// post-decrypt detection. The UI branches on it only for the action column (server
// rows carry an enforcement `action`; local rows are blank).
type QueueRowSource uint

const (
	// A nest-issued [`ObligationAction`] (mail-ingest / admin enforcement, appeals,
	// or plaintext-mode social labels).
	QueueRowSourceServer QueueRowSource = 1
	// A client-side post-decrypt [`LocalDetection`] — blank action column.
	QueueRowSourceLocal QueueRowSource = 2
)

type FfiConverterQueueRowSource struct{}

var FfiConverterQueueRowSourceINSTANCE = FfiConverterQueueRowSource{}

func (c FfiConverterQueueRowSource) Lift(rb RustBufferI) QueueRowSource {
	return LiftFromRustBuffer[QueueRowSource](c, rb)
}

func (c FfiConverterQueueRowSource) Lower(value QueueRowSource) C.RustBuffer {
	return LowerIntoRustBuffer[QueueRowSource](c, value)
}

func (c FfiConverterQueueRowSource) LowerExternal(value QueueRowSource) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[QueueRowSource](c, value))
}
func (FfiConverterQueueRowSource) Read(reader io.Reader) QueueRowSource {
	id := readInt32(reader)
	return QueueRowSource(id)
}

func (FfiConverterQueueRowSource) Write(writer io.Writer, value QueueRowSource) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerQueueRowSource struct{}

func (_ FfiDestroyerQueueRowSource) Destroy(value QueueRowSource) {
}

type FfiConverterOptionalUint8 struct{}

var FfiConverterOptionalUint8INSTANCE = FfiConverterOptionalUint8{}

func (c FfiConverterOptionalUint8) Lift(rb RustBufferI) *uint8 {
	return LiftFromRustBuffer[*uint8](c, rb)
}

func (_ FfiConverterOptionalUint8) Read(reader io.Reader) *uint8 {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterUint8INSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalUint8) Lower(value *uint8) C.RustBuffer {
	return LowerIntoRustBuffer[*uint8](c, value)
}

func (c FfiConverterOptionalUint8) LowerExternal(value *uint8) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*uint8](c, value))
}

func (_ FfiConverterOptionalUint8) Write(writer io.Writer, value *uint8) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterUint8INSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalUint8 struct{}

func (_ FfiDestroyerOptionalUint8) Destroy(value *uint8) {
	if value != nil {
		FfiDestroyerUint8{}.Destroy(*value)
	}
}
