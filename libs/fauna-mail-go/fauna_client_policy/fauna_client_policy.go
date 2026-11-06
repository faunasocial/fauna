package fauna_client_policy

// #include <fauna_client_policy.h>
import "C"

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"
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
		C.ffi_fauna_client_policy_rustbuffer_free(cb.inner, status)
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
		return C.ffi_fauna_client_policy_rustbuffer_from_bytes(foreign, status)
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
		return C.ffi_fauna_client_policy_uniffi_contract_version()
	})
	if bindingsContractVersion != int(scaffoldingContractVersion) {
		// If this happens try cleaning and rebuilding your project
		panic("fauna_client_policy: UniFFI contract version mismatch")
	}
}

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

type FfiConverterUint64 struct{}

var FfiConverterUint64INSTANCE = FfiConverterUint64{}

func (FfiConverterUint64) Lower(value uint64) C.uint64_t {
	return C.uint64_t(value)
}

func (FfiConverterUint64) Write(writer io.Writer, value uint64) {
	writeUint64(writer, value)
}

func (FfiConverterUint64) Lift(value C.uint64_t) uint64 {
	return uint64(value)
}

func (FfiConverterUint64) Read(reader io.Reader) uint64 {
	return readUint64(reader)
}

type FfiDestroyerUint64 struct{}

func (FfiDestroyerUint64) Destroy(_ uint64) {}

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

type FfiConverterBool struct{}

var FfiConverterBoolINSTANCE = FfiConverterBool{}

func (FfiConverterBool) Lower(value bool) C.int8_t {
	if value {
		return C.int8_t(1)
	}
	return C.int8_t(0)
}

func (FfiConverterBool) Write(writer io.Writer, value bool) {
	if value {
		writeInt8(writer, 1)
	} else {
		writeInt8(writer, 0)
	}
}

func (FfiConverterBool) Lift(value C.int8_t) bool {
	return value != 0
}

func (FfiConverterBool) Read(reader io.Reader) bool {
	return readInt8(reader) != 0
}

type FfiDestroyerBool struct{}

func (FfiDestroyerBool) Destroy(_ bool) {}

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

type CategoryOption struct {
	Category string
	Label    fauna_core.LocalizedText
}

func (r *CategoryOption) Destroy() {
	FfiDestroyerString{}.Destroy(r.Category)
	fauna_core.FfiDestroyerLocalizedText{}.Destroy(r.Label)
}

type FfiConverterCategoryOption struct{}

var FfiConverterCategoryOptionINSTANCE = FfiConverterCategoryOption{}

func (c FfiConverterCategoryOption) Lift(rb RustBufferI) CategoryOption {
	return LiftFromRustBuffer[CategoryOption](c, rb)
}

func (c FfiConverterCategoryOption) Read(reader io.Reader) CategoryOption {
	return CategoryOption{
		FfiConverterStringINSTANCE.Read(reader),
		fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterCategoryOption) Lower(value CategoryOption) C.RustBuffer {
	return LowerIntoRustBuffer[CategoryOption](c, value)
}

func (c FfiConverterCategoryOption) LowerExternal(value CategoryOption) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[CategoryOption](c, value))
}

func (c FfiConverterCategoryOption) Write(writer io.Writer, value CategoryOption) {
	FfiConverterStringINSTANCE.Write(writer, value.Category)
	fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, value.Label)
}

type FfiDestroyerCategoryOption struct{}

func (_ FfiDestroyerCategoryOption) Destroy(value CategoryOption) {
	value.Destroy()
}

type KindOption struct {
	Kind  RuleKind
	Label fauna_core.LocalizedText
}

func (r *KindOption) Destroy() {
	FfiDestroyerRuleKind{}.Destroy(r.Kind)
	fauna_core.FfiDestroyerLocalizedText{}.Destroy(r.Label)
}

type FfiConverterKindOption struct{}

var FfiConverterKindOptionINSTANCE = FfiConverterKindOption{}

func (c FfiConverterKindOption) Lift(rb RustBufferI) KindOption {
	return LiftFromRustBuffer[KindOption](c, rb)
}

func (c FfiConverterKindOption) Read(reader io.Reader) KindOption {
	return KindOption{
		FfiConverterRuleKindINSTANCE.Read(reader),
		fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterKindOption) Lower(value KindOption) C.RustBuffer {
	return LowerIntoRustBuffer[KindOption](c, value)
}

func (c FfiConverterKindOption) LowerExternal(value KindOption) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[KindOption](c, value))
}

func (c FfiConverterKindOption) Write(writer io.Writer, value KindOption) {
	FfiConverterRuleKindINSTANCE.Write(writer, value.Kind)
	fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, value.Label)
}

type FfiDestroyerKindOption struct{}

func (_ FfiDestroyerKindOption) Destroy(value KindOption) {
	value.Destroy()
}

// One option of the labeler picker ([`RuleEditor::labeler_choices`]).
type LabelerChoice struct {
	// The scorer name the option paints and the rule names.
	Name string
	// `None` for a labeler the document bundles
	// ([`RuleEditor::choose_bundled_labeler`]); the hex labeler id of a
	// subscribed one, whose module the mounting fetches
	// (`fauna_client_labelers::LabelersClient::rule_module`) and hands to
	// [`RuleEditor::set_labeler`] under [`Self::name`].
	LabelerId *string
}

func (r *LabelerChoice) Destroy() {
	FfiDestroyerString{}.Destroy(r.Name)
	FfiDestroyerOptionalString{}.Destroy(r.LabelerId)
}

type FfiConverterLabelerChoice struct{}

var FfiConverterLabelerChoiceINSTANCE = FfiConverterLabelerChoice{}

func (c FfiConverterLabelerChoice) Lift(rb RustBufferI) LabelerChoice {
	return LiftFromRustBuffer[LabelerChoice](c, rb)
}

func (c FfiConverterLabelerChoice) Read(reader io.Reader) LabelerChoice {
	return LabelerChoice{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterLabelerChoice) Lower(value LabelerChoice) C.RustBuffer {
	return LowerIntoRustBuffer[LabelerChoice](c, value)
}

func (c FfiConverterLabelerChoice) LowerExternal(value LabelerChoice) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[LabelerChoice](c, value))
}

func (c FfiConverterLabelerChoice) Write(writer io.Writer, value LabelerChoice) {
	FfiConverterStringINSTANCE.Write(writer, value.Name)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.LabelerId)
}

type FfiDestroyerLabelerChoice struct{}

func (_ FfiDestroyerLabelerChoice) Destroy(value LabelerChoice) {
	value.Destroy()
}

type LabelerOption struct {
	Name string
}

func (r *LabelerOption) Destroy() {
	FfiDestroyerString{}.Destroy(r.Name)
}

type FfiConverterLabelerOption struct{}

var FfiConverterLabelerOptionINSTANCE = FfiConverterLabelerOption{}

func (c FfiConverterLabelerOption) Lift(rb RustBufferI) LabelerOption {
	return LiftFromRustBuffer[LabelerOption](c, rb)
}

func (c FfiConverterLabelerOption) Read(reader io.Reader) LabelerOption {
	return LabelerOption{
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterLabelerOption) Lower(value LabelerOption) C.RustBuffer {
	return LowerIntoRustBuffer[LabelerOption](c, value)
}

func (c FfiConverterLabelerOption) LowerExternal(value LabelerOption) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[LabelerOption](c, value))
}

func (c FfiConverterLabelerOption) Write(writer io.Writer, value LabelerOption) {
	FfiConverterStringINSTANCE.Write(writer, value.Name)
}

type FfiDestroyerLabelerOption struct{}

func (_ FfiDestroyerLabelerOption) Destroy(value LabelerOption) {
	value.Destroy()
}

type LevelOption struct {
	Label    fauna_core.LocalizedText
	Permille uint16
}

func (r *LevelOption) Destroy() {
	fauna_core.FfiDestroyerLocalizedText{}.Destroy(r.Label)
	FfiDestroyerUint16{}.Destroy(r.Permille)
}

type FfiConverterLevelOption struct{}

var FfiConverterLevelOptionINSTANCE = FfiConverterLevelOption{}

func (c FfiConverterLevelOption) Lift(rb RustBufferI) LevelOption {
	return LiftFromRustBuffer[LevelOption](c, rb)
}

func (c FfiConverterLevelOption) Read(reader io.Reader) LevelOption {
	return LevelOption{
		fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterUint16INSTANCE.Read(reader),
	}
}

func (c FfiConverterLevelOption) Lower(value LevelOption) C.RustBuffer {
	return LowerIntoRustBuffer[LevelOption](c, value)
}

func (c FfiConverterLevelOption) LowerExternal(value LevelOption) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[LevelOption](c, value))
}

func (c FfiConverterLevelOption) Write(writer io.Writer, value LevelOption) {
	fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, value.Label)
	FfiConverterUint16INSTANCE.Write(writer, value.Permille)
}

type FfiDestroyerLevelOption struct{}

func (_ FfiDestroyerLevelOption) Destroy(value LevelOption) {
	value.Destroy()
}

// One rule of a [`NestRulesSummary`].
type NestRuleLine struct {
	Verdict fauna_core.LocalizedText
	// `None` where a level means nothing (a keywords rule).
	Level *fauna_core.LocalizedText
	// The nest's own words in the member's language where the document has
	// them, else its default. Shown verbatim, never translated.
	Reason string
	// "Not applied on this device" — a rule this build cannot enforce.
	Inert *fauna_core.LocalizedText
}

func (r *NestRuleLine) Destroy() {
	fauna_core.FfiDestroyerLocalizedText{}.Destroy(r.Verdict)
	FfiDestroyerOptionalLocalizedText{}.Destroy(r.Level)
	FfiDestroyerString{}.Destroy(r.Reason)
	FfiDestroyerOptionalLocalizedText{}.Destroy(r.Inert)
}

type FfiConverterNestRuleLine struct{}

var FfiConverterNestRuleLineINSTANCE = FfiConverterNestRuleLine{}

func (c FfiConverterNestRuleLine) Lift(rb RustBufferI) NestRuleLine {
	return LiftFromRustBuffer[NestRuleLine](c, rb)
}

func (c FfiConverterNestRuleLine) Read(reader io.Reader) NestRuleLine {
	return NestRuleLine{
		fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterOptionalLocalizedTextINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterNestRuleLine) Lower(value NestRuleLine) C.RustBuffer {
	return LowerIntoRustBuffer[NestRuleLine](c, value)
}

func (c FfiConverterNestRuleLine) LowerExternal(value NestRuleLine) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[NestRuleLine](c, value))
}

func (c FfiConverterNestRuleLine) Write(writer io.Writer, value NestRuleLine) {
	fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, value.Verdict)
	FfiConverterOptionalLocalizedTextINSTANCE.Write(writer, value.Level)
	FfiConverterStringINSTANCE.Write(writer, value.Reason)
	FfiConverterOptionalLocalizedTextINSTANCE.Write(writer, value.Inert)
}

type FfiDestroyerNestRuleLine struct{}

func (_ FfiDestroyerNestRuleLine) Destroy(value NestRuleLine) {
	value.Destroy()
}

// A nest's rules as the joining surface (`onboarding-nest-rules`) and the
// settings rules surface (`settings-nest-rules-*`) show them: per rule the
// verdict, the level and the nest's own reason, every label already chosen.
type NestRulesSummary struct {
	Domain   string
	Sequence uint64
	IssuedAt int64
	// Empty for a nest with no rules, and for a document this build cannot
	// decode; whether a surface paints is [`Self::is_shown`].
	Rules []NestRuleLine
	// Why none of the document applies on this device; `None` when it
	// applies.
	NotApplied *NestRulesNotApplied
	// [`NestRulesNotApplied::sentence`] of `not_applied`, carried as data so
	// an app across the UniFFI or wasm boundary only paints it.
	NotAppliedSentence *fauna_core.LocalizedText
}

func (r *NestRulesSummary) Destroy() {
	FfiDestroyerString{}.Destroy(r.Domain)
	FfiDestroyerUint64{}.Destroy(r.Sequence)
	FfiDestroyerInt64{}.Destroy(r.IssuedAt)
	FfiDestroyerSequenceNestRuleLine{}.Destroy(r.Rules)
	FfiDestroyerOptionalNestRulesNotApplied{}.Destroy(r.NotApplied)
	FfiDestroyerOptionalLocalizedText{}.Destroy(r.NotAppliedSentence)
}

type FfiConverterNestRulesSummary struct{}

var FfiConverterNestRulesSummaryINSTANCE = FfiConverterNestRulesSummary{}

func (c FfiConverterNestRulesSummary) Lift(rb RustBufferI) NestRulesSummary {
	return LiftFromRustBuffer[NestRulesSummary](c, rb)
}

func (c FfiConverterNestRulesSummary) Read(reader io.Reader) NestRulesSummary {
	return NestRulesSummary{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterSequenceNestRuleLineINSTANCE.Read(reader),
		FfiConverterOptionalNestRulesNotAppliedINSTANCE.Read(reader),
		FfiConverterOptionalLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterNestRulesSummary) Lower(value NestRulesSummary) C.RustBuffer {
	return LowerIntoRustBuffer[NestRulesSummary](c, value)
}

func (c FfiConverterNestRulesSummary) LowerExternal(value NestRulesSummary) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[NestRulesSummary](c, value))
}

func (c FfiConverterNestRulesSummary) Write(writer io.Writer, value NestRulesSummary) {
	FfiConverterStringINSTANCE.Write(writer, value.Domain)
	FfiConverterUint64INSTANCE.Write(writer, value.Sequence)
	FfiConverterInt64INSTANCE.Write(writer, value.IssuedAt)
	FfiConverterSequenceNestRuleLineINSTANCE.Write(writer, value.Rules)
	FfiConverterOptionalNestRulesNotAppliedINSTANCE.Write(writer, value.NotApplied)
	FfiConverterOptionalLocalizedTextINSTANCE.Write(writer, value.NotAppliedSentence)
}

type FfiDestroyerNestRulesSummary struct{}

func (_ FfiDestroyerNestRulesSummary) Destroy(value NestRulesSummary) {
	value.Destroy()
}

type ReasonEntry struct {
	Lang string
	Text string
}

func (r *ReasonEntry) Destroy() {
	FfiDestroyerString{}.Destroy(r.Lang)
	FfiDestroyerString{}.Destroy(r.Text)
}

type FfiConverterReasonEntry struct{}

var FfiConverterReasonEntryINSTANCE = FfiConverterReasonEntry{}

func (c FfiConverterReasonEntry) Lift(rb RustBufferI) ReasonEntry {
	return LiftFromRustBuffer[ReasonEntry](c, rb)
}

func (c FfiConverterReasonEntry) Read(reader io.Reader) ReasonEntry {
	return ReasonEntry{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterReasonEntry) Lower(value ReasonEntry) C.RustBuffer {
	return LowerIntoRustBuffer[ReasonEntry](c, value)
}

func (c FfiConverterReasonEntry) LowerExternal(value ReasonEntry) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ReasonEntry](c, value))
}

func (c FfiConverterReasonEntry) Write(writer io.Writer, value ReasonEntry) {
	FfiConverterStringINSTANCE.Write(writer, value.Lang)
	FfiConverterStringINSTANCE.Write(writer, value.Text)
}

type FfiDestroyerReasonEntry struct{}

func (_ FfiDestroyerReasonEntry) Destroy(value ReasonEntry) {
	value.Destroy()
}

// Everything the `rule-editor` component paints.
type RuleEditorView struct {
	// One per rule, in document order (`rule-editor-item`).
	Rows []RuleRow
	// The kinds `rule-editor-item-kind-select` and the add menu offer.
	Kinds []KindOption
	// `rule-editor-item-category-select`'s options.
	Categories []CategoryOption
	// The labelers the document bundles — `rule-editor-item-labeler-select`
	// offers these beside the publisher's subscriptions the app lists.
	Labelers []LabelerOption
	// `rule-editor-item-verdict-select`'s options.
	Verdicts []VerdictOption
	// `rule-editor-item-level-select`'s options; a row's `level_index`
	// indexes this list.
	Levels []LevelOption
	// What stops a save, as sentences (`rule-editor-status`). Empty when the
	// document may be saved.
	Defects []fauna_core.LocalizedText
	// False for a document a newer build wrote: shown, never edited.
	Editable bool
	// Whether `rule-editor-save-button` is enabled.
	CanSave bool
	// The reason language every rule requires.
	ReasonDefaultKey string
}

func (r *RuleEditorView) Destroy() {
	FfiDestroyerSequenceRuleRow{}.Destroy(r.Rows)
	FfiDestroyerSequenceKindOption{}.Destroy(r.Kinds)
	FfiDestroyerSequenceCategoryOption{}.Destroy(r.Categories)
	FfiDestroyerSequenceLabelerOption{}.Destroy(r.Labelers)
	FfiDestroyerSequenceVerdictOption{}.Destroy(r.Verdicts)
	FfiDestroyerSequenceLevelOption{}.Destroy(r.Levels)
	FfiDestroyerSequenceLocalizedText{}.Destroy(r.Defects)
	FfiDestroyerBool{}.Destroy(r.Editable)
	FfiDestroyerBool{}.Destroy(r.CanSave)
	FfiDestroyerString{}.Destroy(r.ReasonDefaultKey)
}

type FfiConverterRuleEditorView struct{}

var FfiConverterRuleEditorViewINSTANCE = FfiConverterRuleEditorView{}

func (c FfiConverterRuleEditorView) Lift(rb RustBufferI) RuleEditorView {
	return LiftFromRustBuffer[RuleEditorView](c, rb)
}

func (c FfiConverterRuleEditorView) Read(reader io.Reader) RuleEditorView {
	return RuleEditorView{
		FfiConverterSequenceRuleRowINSTANCE.Read(reader),
		FfiConverterSequenceKindOptionINSTANCE.Read(reader),
		FfiConverterSequenceCategoryOptionINSTANCE.Read(reader),
		FfiConverterSequenceLabelerOptionINSTANCE.Read(reader),
		FfiConverterSequenceVerdictOptionINSTANCE.Read(reader),
		FfiConverterSequenceLevelOptionINSTANCE.Read(reader),
		FfiConverterSequenceLocalizedTextINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterRuleEditorView) Lower(value RuleEditorView) C.RustBuffer {
	return LowerIntoRustBuffer[RuleEditorView](c, value)
}

func (c FfiConverterRuleEditorView) LowerExternal(value RuleEditorView) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RuleEditorView](c, value))
}

func (c FfiConverterRuleEditorView) Write(writer io.Writer, value RuleEditorView) {
	FfiConverterSequenceRuleRowINSTANCE.Write(writer, value.Rows)
	FfiConverterSequenceKindOptionINSTANCE.Write(writer, value.Kinds)
	FfiConverterSequenceCategoryOptionINSTANCE.Write(writer, value.Categories)
	FfiConverterSequenceLabelerOptionINSTANCE.Write(writer, value.Labelers)
	FfiConverterSequenceVerdictOptionINSTANCE.Write(writer, value.Verdicts)
	FfiConverterSequenceLevelOptionINSTANCE.Write(writer, value.Levels)
	FfiConverterSequenceLocalizedTextINSTANCE.Write(writer, value.Defects)
	FfiConverterBoolINSTANCE.Write(writer, value.Editable)
	FfiConverterBoolINSTANCE.Write(writer, value.CanSave)
	FfiConverterStringINSTANCE.Write(writer, value.ReasonDefaultKey)
}

type FfiDestroyerRuleEditorView struct{}

func (_ FfiDestroyerRuleEditorView) Destroy(value RuleEditorView) {
	value.Destroy()
}

// One rule. Exactly one of `category` / `keywords` / `labeler` / `prompt` is
// set, by `kind`; a `list` row sets none of them.
type RuleRow struct {
	// 1-based — the number every defect sentence names.
	Number    uint32
	Kind      RuleKind
	KindLabel fauna_core.LocalizedText
	// A loaded `list` rule: shown, removable, never edited.
	ReadOnly bool
	Category *string
	// The terms, one per line.
	Keywords *string
	// The labeler's name; empty until one is chosen.
	Labeler *string
	Prompt  *string
	Verdict RuleVerdict
	// Into [`RuleEditorView::levels`]; `None` where a level means nothing (a
	// keywords rule). A stored per-mille between two levels shows as the
	// nearer and is kept until the user picks one.
	LevelIndex *uint32
	// The publisher's own reason texts, the required default first.
	Reasons    []ReasonEntry
	ReasonCode string
	// "Not applied on this device" — a rule this build cannot enforce.
	Inert *fauna_core.LocalizedText
}

func (r *RuleRow) Destroy() {
	FfiDestroyerUint32{}.Destroy(r.Number)
	FfiDestroyerRuleKind{}.Destroy(r.Kind)
	fauna_core.FfiDestroyerLocalizedText{}.Destroy(r.KindLabel)
	FfiDestroyerBool{}.Destroy(r.ReadOnly)
	FfiDestroyerOptionalString{}.Destroy(r.Category)
	FfiDestroyerOptionalString{}.Destroy(r.Keywords)
	FfiDestroyerOptionalString{}.Destroy(r.Labeler)
	FfiDestroyerOptionalString{}.Destroy(r.Prompt)
	FfiDestroyerRuleVerdict{}.Destroy(r.Verdict)
	FfiDestroyerOptionalUint32{}.Destroy(r.LevelIndex)
	FfiDestroyerSequenceReasonEntry{}.Destroy(r.Reasons)
	FfiDestroyerString{}.Destroy(r.ReasonCode)
	FfiDestroyerOptionalLocalizedText{}.Destroy(r.Inert)
}

type FfiConverterRuleRow struct{}

var FfiConverterRuleRowINSTANCE = FfiConverterRuleRow{}

func (c FfiConverterRuleRow) Lift(rb RustBufferI) RuleRow {
	return LiftFromRustBuffer[RuleRow](c, rb)
}

func (c FfiConverterRuleRow) Read(reader io.Reader) RuleRow {
	return RuleRow{
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterRuleKindINSTANCE.Read(reader),
		fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterRuleVerdictINSTANCE.Read(reader),
		FfiConverterOptionalUint32INSTANCE.Read(reader),
		FfiConverterSequenceReasonEntryINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterRuleRow) Lower(value RuleRow) C.RustBuffer {
	return LowerIntoRustBuffer[RuleRow](c, value)
}

func (c FfiConverterRuleRow) LowerExternal(value RuleRow) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RuleRow](c, value))
}

func (c FfiConverterRuleRow) Write(writer io.Writer, value RuleRow) {
	FfiConverterUint32INSTANCE.Write(writer, value.Number)
	FfiConverterRuleKindINSTANCE.Write(writer, value.Kind)
	fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, value.KindLabel)
	FfiConverterBoolINSTANCE.Write(writer, value.ReadOnly)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Category)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Keywords)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Labeler)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Prompt)
	FfiConverterRuleVerdictINSTANCE.Write(writer, value.Verdict)
	FfiConverterOptionalUint32INSTANCE.Write(writer, value.LevelIndex)
	FfiConverterSequenceReasonEntryINSTANCE.Write(writer, value.Reasons)
	FfiConverterStringINSTANCE.Write(writer, value.ReasonCode)
	FfiConverterOptionalLocalizedTextINSTANCE.Write(writer, value.Inert)
}

type FfiDestroyerRuleRow struct{}

func (_ FfiDestroyerRuleRow) Destroy(value RuleRow) {
	value.Destroy()
}

// One accepted signed record, in the form an app persists between runs
// ([`NestRulesBook::stored`] / [`NestRulesBook::restore`]). Kept signed, so a
// restore verifies again rather than trusting the app's storage.
type StoredNestRules struct {
	// 64-char lowercase hex of the nest's identity key.
	NestId string
	// The nest's domain as its self-description named it.
	Domain    string
	Document  []byte
	Sequence  uint64
	IssuedAt  int64
	Signature []byte
}

func (r *StoredNestRules) Destroy() {
	FfiDestroyerString{}.Destroy(r.NestId)
	FfiDestroyerString{}.Destroy(r.Domain)
	FfiDestroyerBytes{}.Destroy(r.Document)
	FfiDestroyerUint64{}.Destroy(r.Sequence)
	FfiDestroyerInt64{}.Destroy(r.IssuedAt)
	FfiDestroyerBytes{}.Destroy(r.Signature)
}

type FfiConverterStoredNestRules struct{}

var FfiConverterStoredNestRulesINSTANCE = FfiConverterStoredNestRules{}

func (c FfiConverterStoredNestRules) Lift(rb RustBufferI) StoredNestRules {
	return LiftFromRustBuffer[StoredNestRules](c, rb)
}

func (c FfiConverterStoredNestRules) Read(reader io.Reader) StoredNestRules {
	return StoredNestRules{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
	}
}

func (c FfiConverterStoredNestRules) Lower(value StoredNestRules) C.RustBuffer {
	return LowerIntoRustBuffer[StoredNestRules](c, value)
}

func (c FfiConverterStoredNestRules) LowerExternal(value StoredNestRules) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[StoredNestRules](c, value))
}

func (c FfiConverterStoredNestRules) Write(writer io.Writer, value StoredNestRules) {
	FfiConverterStringINSTANCE.Write(writer, value.NestId)
	FfiConverterStringINSTANCE.Write(writer, value.Domain)
	FfiConverterBytesINSTANCE.Write(writer, value.Document)
	FfiConverterUint64INSTANCE.Write(writer, value.Sequence)
	FfiConverterInt64INSTANCE.Write(writer, value.IssuedAt)
	FfiConverterBytesINSTANCE.Write(writer, value.Signature)
}

type FfiDestroyerStoredNestRules struct{}

func (_ FfiDestroyerStoredNestRules) Destroy(value StoredNestRules) {
	value.Destroy()
}

type VerdictOption struct {
	Verdict RuleVerdict
	Label   fauna_core.LocalizedText
}

func (r *VerdictOption) Destroy() {
	FfiDestroyerRuleVerdict{}.Destroy(r.Verdict)
	fauna_core.FfiDestroyerLocalizedText{}.Destroy(r.Label)
}

type FfiConverterVerdictOption struct{}

var FfiConverterVerdictOptionINSTANCE = FfiConverterVerdictOption{}

func (c FfiConverterVerdictOption) Lift(rb RustBufferI) VerdictOption {
	return LiftFromRustBuffer[VerdictOption](c, rb)
}

func (c FfiConverterVerdictOption) Read(reader io.Reader) VerdictOption {
	return VerdictOption{
		FfiConverterRuleVerdictINSTANCE.Read(reader),
		fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterVerdictOption) Lower(value VerdictOption) C.RustBuffer {
	return LowerIntoRustBuffer[VerdictOption](c, value)
}

func (c FfiConverterVerdictOption) LowerExternal(value VerdictOption) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[VerdictOption](c, value))
}

func (c FfiConverterVerdictOption) Write(writer io.Writer, value VerdictOption) {
	FfiConverterRuleVerdictINSTANCE.Write(writer, value.Verdict)
	fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, value.Label)
}

type FfiDestroyerVerdictOption struct{}

func (_ FfiDestroyerVerdictOption) Destroy(value VerdictOption) {
	value.Destroy()
}

// Why none of a nest's document applies on this device. The sentence for each
// ([`Self::sentence`]) is painted under the heading on the joining surface and
// on the settings rules surface; shown before sign-in, it is a public text
// (`docs/goal/architecture/public-texts.md` § The boundary).
type NestRulesNotApplied uint

const (
	// A newer grammar than this build implements: updating the app applies
	// them.
	NestRulesNotAppliedNewerVersion NestRulesNotApplied = 1
	// Structurally wrong — the nest validates before storing, so this is a
	// defence, not an expected state.
	NestRulesNotAppliedMalformed NestRulesNotApplied = 2
)

type FfiConverterNestRulesNotApplied struct{}

var FfiConverterNestRulesNotAppliedINSTANCE = FfiConverterNestRulesNotApplied{}

func (c FfiConverterNestRulesNotApplied) Lift(rb RustBufferI) NestRulesNotApplied {
	return LiftFromRustBuffer[NestRulesNotApplied](c, rb)
}

func (c FfiConverterNestRulesNotApplied) Lower(value NestRulesNotApplied) C.RustBuffer {
	return LowerIntoRustBuffer[NestRulesNotApplied](c, value)
}

func (c FfiConverterNestRulesNotApplied) LowerExternal(value NestRulesNotApplied) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[NestRulesNotApplied](c, value))
}
func (FfiConverterNestRulesNotApplied) Read(reader io.Reader) NestRulesNotApplied {
	id := readInt32(reader)
	return NestRulesNotApplied(id)
}

func (FfiConverterNestRulesNotApplied) Write(writer io.Writer, value NestRulesNotApplied) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerNestRulesNotApplied struct{}

func (_ FfiDestroyerNestRulesNotApplied) Destroy(value NestRulesNotApplied) {
}

// What a rule tests. The editor's own vocabulary over the grammar: a rule is
// a **category** rule when its factor is a label category, and otherwise the
// kind of the bundled scorer it names.
type RuleKind uint

const (
	// One of the canonical categories (`nsfw`, `spam`, `phishing`,
	// `commercial`), as the device's labelers produce them.
	RuleKindCategory RuleKind = 1
	// A list of terms, matched by the shared keyword matcher.
	RuleKindKeywords RuleKind = 2
	// A labeler the publisher bundles as a `wasm` scorer.
	RuleKindLabeler RuleKind = 3
	// A prompt for an on-device language model — inert on every build today.
	RuleKindPrompt RuleKind = 4
	// A content-id list. Never offered; a loaded region rule is read-only.
	RuleKindList RuleKind = 5
)

type FfiConverterRuleKind struct{}

var FfiConverterRuleKindINSTANCE = FfiConverterRuleKind{}

func (c FfiConverterRuleKind) Lift(rb RustBufferI) RuleKind {
	return LiftFromRustBuffer[RuleKind](c, rb)
}

func (c FfiConverterRuleKind) Lower(value RuleKind) C.RustBuffer {
	return LowerIntoRustBuffer[RuleKind](c, value)
}

func (c FfiConverterRuleKind) LowerExternal(value RuleKind) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RuleKind](c, value))
}
func (FfiConverterRuleKind) Read(reader io.Reader) RuleKind {
	id := readInt32(reader)
	return RuleKind(id)
}

func (FfiConverterRuleKind) Write(writer io.Writer, value RuleKind) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerRuleKind struct{}

func (_ FfiDestroyerRuleKind) Destroy(value RuleKind) {
}

// The two verdicts a rule may carry — the engine's two render verbs, and no
// other (`nest-rules.md` § The shared rule editor).
type RuleVerdict uint

const (
	RuleVerdictBlock    RuleVerdict = 1
	RuleVerdictCollapse RuleVerdict = 2
)

type FfiConverterRuleVerdict struct{}

var FfiConverterRuleVerdictINSTANCE = FfiConverterRuleVerdict{}

func (c FfiConverterRuleVerdict) Lift(rb RustBufferI) RuleVerdict {
	return LiftFromRustBuffer[RuleVerdict](c, rb)
}

func (c FfiConverterRuleVerdict) Lower(value RuleVerdict) C.RustBuffer {
	return LowerIntoRustBuffer[RuleVerdict](c, value)
}

func (c FfiConverterRuleVerdict) LowerExternal(value RuleVerdict) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RuleVerdict](c, value))
}
func (FfiConverterRuleVerdict) Read(reader io.Reader) RuleVerdict {
	id := readInt32(reader)
	return RuleVerdict(id)
}

func (FfiConverterRuleVerdict) Write(writer io.Writer, value RuleVerdict) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerRuleVerdict struct{}

func (_ FfiDestroyerRuleVerdict) Destroy(value RuleVerdict) {
}

type FfiConverterOptionalUint32 struct{}

var FfiConverterOptionalUint32INSTANCE = FfiConverterOptionalUint32{}

func (c FfiConverterOptionalUint32) Lift(rb RustBufferI) *uint32 {
	return LiftFromRustBuffer[*uint32](c, rb)
}

func (_ FfiConverterOptionalUint32) Read(reader io.Reader) *uint32 {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterUint32INSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalUint32) Lower(value *uint32) C.RustBuffer {
	return LowerIntoRustBuffer[*uint32](c, value)
}

func (c FfiConverterOptionalUint32) LowerExternal(value *uint32) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*uint32](c, value))
}

func (_ FfiConverterOptionalUint32) Write(writer io.Writer, value *uint32) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterUint32INSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalUint32 struct{}

func (_ FfiDestroyerOptionalUint32) Destroy(value *uint32) {
	if value != nil {
		FfiDestroyerUint32{}.Destroy(*value)
	}
}

type FfiConverterOptionalString struct{}

var FfiConverterOptionalStringINSTANCE = FfiConverterOptionalString{}

func (c FfiConverterOptionalString) Lift(rb RustBufferI) *string {
	return LiftFromRustBuffer[*string](c, rb)
}

func (_ FfiConverterOptionalString) Read(reader io.Reader) *string {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterStringINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalString) Lower(value *string) C.RustBuffer {
	return LowerIntoRustBuffer[*string](c, value)
}

func (c FfiConverterOptionalString) LowerExternal(value *string) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*string](c, value))
}

func (_ FfiConverterOptionalString) Write(writer io.Writer, value *string) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterStringINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalString struct{}

func (_ FfiDestroyerOptionalString) Destroy(value *string) {
	if value != nil {
		FfiDestroyerString{}.Destroy(*value)
	}
}

type FfiConverterOptionalLocalizedText struct{}

var FfiConverterOptionalLocalizedTextINSTANCE = FfiConverterOptionalLocalizedText{}

func (c FfiConverterOptionalLocalizedText) Lift(rb RustBufferI) *fauna_core.LocalizedText {
	return LiftFromRustBuffer[*fauna_core.LocalizedText](c, rb)
}

func (_ FfiConverterOptionalLocalizedText) Read(reader io.Reader) *fauna_core.LocalizedText {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalLocalizedText) Lower(value *fauna_core.LocalizedText) C.RustBuffer {
	return LowerIntoRustBuffer[*fauna_core.LocalizedText](c, value)
}

func (c FfiConverterOptionalLocalizedText) LowerExternal(value *fauna_core.LocalizedText) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*fauna_core.LocalizedText](c, value))
}

func (_ FfiConverterOptionalLocalizedText) Write(writer io.Writer, value *fauna_core.LocalizedText) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalLocalizedText struct{}

func (_ FfiDestroyerOptionalLocalizedText) Destroy(value *fauna_core.LocalizedText) {
	if value != nil {
		fauna_core.FfiDestroyerLocalizedText{}.Destroy(*value)
	}
}

type FfiConverterOptionalNestRulesNotApplied struct{}

var FfiConverterOptionalNestRulesNotAppliedINSTANCE = FfiConverterOptionalNestRulesNotApplied{}

func (c FfiConverterOptionalNestRulesNotApplied) Lift(rb RustBufferI) *NestRulesNotApplied {
	return LiftFromRustBuffer[*NestRulesNotApplied](c, rb)
}

func (_ FfiConverterOptionalNestRulesNotApplied) Read(reader io.Reader) *NestRulesNotApplied {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterNestRulesNotAppliedINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalNestRulesNotApplied) Lower(value *NestRulesNotApplied) C.RustBuffer {
	return LowerIntoRustBuffer[*NestRulesNotApplied](c, value)
}

func (c FfiConverterOptionalNestRulesNotApplied) LowerExternal(value *NestRulesNotApplied) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*NestRulesNotApplied](c, value))
}

func (_ FfiConverterOptionalNestRulesNotApplied) Write(writer io.Writer, value *NestRulesNotApplied) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterNestRulesNotAppliedINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalNestRulesNotApplied struct{}

func (_ FfiDestroyerOptionalNestRulesNotApplied) Destroy(value *NestRulesNotApplied) {
	if value != nil {
		FfiDestroyerNestRulesNotApplied{}.Destroy(*value)
	}
}

type FfiConverterSequenceCategoryOption struct{}

var FfiConverterSequenceCategoryOptionINSTANCE = FfiConverterSequenceCategoryOption{}

func (c FfiConverterSequenceCategoryOption) Lift(rb RustBufferI) []CategoryOption {
	return LiftFromRustBuffer[[]CategoryOption](c, rb)
}

func (c FfiConverterSequenceCategoryOption) Read(reader io.Reader) []CategoryOption {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]CategoryOption, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterCategoryOptionINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceCategoryOption) Lower(value []CategoryOption) C.RustBuffer {
	return LowerIntoRustBuffer[[]CategoryOption](c, value)
}

func (c FfiConverterSequenceCategoryOption) LowerExternal(value []CategoryOption) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]CategoryOption](c, value))
}

func (c FfiConverterSequenceCategoryOption) Write(writer io.Writer, value []CategoryOption) {
	if len(value) > math.MaxInt32 {
		panic("[]CategoryOption is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterCategoryOptionINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceCategoryOption struct{}

func (FfiDestroyerSequenceCategoryOption) Destroy(sequence []CategoryOption) {
	for _, value := range sequence {
		FfiDestroyerCategoryOption{}.Destroy(value)
	}
}

type FfiConverterSequenceKindOption struct{}

var FfiConverterSequenceKindOptionINSTANCE = FfiConverterSequenceKindOption{}

func (c FfiConverterSequenceKindOption) Lift(rb RustBufferI) []KindOption {
	return LiftFromRustBuffer[[]KindOption](c, rb)
}

func (c FfiConverterSequenceKindOption) Read(reader io.Reader) []KindOption {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]KindOption, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterKindOptionINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceKindOption) Lower(value []KindOption) C.RustBuffer {
	return LowerIntoRustBuffer[[]KindOption](c, value)
}

func (c FfiConverterSequenceKindOption) LowerExternal(value []KindOption) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]KindOption](c, value))
}

func (c FfiConverterSequenceKindOption) Write(writer io.Writer, value []KindOption) {
	if len(value) > math.MaxInt32 {
		panic("[]KindOption is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterKindOptionINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceKindOption struct{}

func (FfiDestroyerSequenceKindOption) Destroy(sequence []KindOption) {
	for _, value := range sequence {
		FfiDestroyerKindOption{}.Destroy(value)
	}
}

type FfiConverterSequenceLabelerOption struct{}

var FfiConverterSequenceLabelerOptionINSTANCE = FfiConverterSequenceLabelerOption{}

func (c FfiConverterSequenceLabelerOption) Lift(rb RustBufferI) []LabelerOption {
	return LiftFromRustBuffer[[]LabelerOption](c, rb)
}

func (c FfiConverterSequenceLabelerOption) Read(reader io.Reader) []LabelerOption {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]LabelerOption, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterLabelerOptionINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceLabelerOption) Lower(value []LabelerOption) C.RustBuffer {
	return LowerIntoRustBuffer[[]LabelerOption](c, value)
}

func (c FfiConverterSequenceLabelerOption) LowerExternal(value []LabelerOption) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]LabelerOption](c, value))
}

func (c FfiConverterSequenceLabelerOption) Write(writer io.Writer, value []LabelerOption) {
	if len(value) > math.MaxInt32 {
		panic("[]LabelerOption is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterLabelerOptionINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceLabelerOption struct{}

func (FfiDestroyerSequenceLabelerOption) Destroy(sequence []LabelerOption) {
	for _, value := range sequence {
		FfiDestroyerLabelerOption{}.Destroy(value)
	}
}

type FfiConverterSequenceLevelOption struct{}

var FfiConverterSequenceLevelOptionINSTANCE = FfiConverterSequenceLevelOption{}

func (c FfiConverterSequenceLevelOption) Lift(rb RustBufferI) []LevelOption {
	return LiftFromRustBuffer[[]LevelOption](c, rb)
}

func (c FfiConverterSequenceLevelOption) Read(reader io.Reader) []LevelOption {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]LevelOption, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterLevelOptionINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceLevelOption) Lower(value []LevelOption) C.RustBuffer {
	return LowerIntoRustBuffer[[]LevelOption](c, value)
}

func (c FfiConverterSequenceLevelOption) LowerExternal(value []LevelOption) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]LevelOption](c, value))
}

func (c FfiConverterSequenceLevelOption) Write(writer io.Writer, value []LevelOption) {
	if len(value) > math.MaxInt32 {
		panic("[]LevelOption is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterLevelOptionINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceLevelOption struct{}

func (FfiDestroyerSequenceLevelOption) Destroy(sequence []LevelOption) {
	for _, value := range sequence {
		FfiDestroyerLevelOption{}.Destroy(value)
	}
}

type FfiConverterSequenceNestRuleLine struct{}

var FfiConverterSequenceNestRuleLineINSTANCE = FfiConverterSequenceNestRuleLine{}

func (c FfiConverterSequenceNestRuleLine) Lift(rb RustBufferI) []NestRuleLine {
	return LiftFromRustBuffer[[]NestRuleLine](c, rb)
}

func (c FfiConverterSequenceNestRuleLine) Read(reader io.Reader) []NestRuleLine {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]NestRuleLine, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterNestRuleLineINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceNestRuleLine) Lower(value []NestRuleLine) C.RustBuffer {
	return LowerIntoRustBuffer[[]NestRuleLine](c, value)
}

func (c FfiConverterSequenceNestRuleLine) LowerExternal(value []NestRuleLine) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]NestRuleLine](c, value))
}

func (c FfiConverterSequenceNestRuleLine) Write(writer io.Writer, value []NestRuleLine) {
	if len(value) > math.MaxInt32 {
		panic("[]NestRuleLine is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterNestRuleLineINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceNestRuleLine struct{}

func (FfiDestroyerSequenceNestRuleLine) Destroy(sequence []NestRuleLine) {
	for _, value := range sequence {
		FfiDestroyerNestRuleLine{}.Destroy(value)
	}
}

type FfiConverterSequenceReasonEntry struct{}

var FfiConverterSequenceReasonEntryINSTANCE = FfiConverterSequenceReasonEntry{}

func (c FfiConverterSequenceReasonEntry) Lift(rb RustBufferI) []ReasonEntry {
	return LiftFromRustBuffer[[]ReasonEntry](c, rb)
}

func (c FfiConverterSequenceReasonEntry) Read(reader io.Reader) []ReasonEntry {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ReasonEntry, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterReasonEntryINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceReasonEntry) Lower(value []ReasonEntry) C.RustBuffer {
	return LowerIntoRustBuffer[[]ReasonEntry](c, value)
}

func (c FfiConverterSequenceReasonEntry) LowerExternal(value []ReasonEntry) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ReasonEntry](c, value))
}

func (c FfiConverterSequenceReasonEntry) Write(writer io.Writer, value []ReasonEntry) {
	if len(value) > math.MaxInt32 {
		panic("[]ReasonEntry is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterReasonEntryINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceReasonEntry struct{}

func (FfiDestroyerSequenceReasonEntry) Destroy(sequence []ReasonEntry) {
	for _, value := range sequence {
		FfiDestroyerReasonEntry{}.Destroy(value)
	}
}

type FfiConverterSequenceRuleRow struct{}

var FfiConverterSequenceRuleRowINSTANCE = FfiConverterSequenceRuleRow{}

func (c FfiConverterSequenceRuleRow) Lift(rb RustBufferI) []RuleRow {
	return LiftFromRustBuffer[[]RuleRow](c, rb)
}

func (c FfiConverterSequenceRuleRow) Read(reader io.Reader) []RuleRow {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]RuleRow, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterRuleRowINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceRuleRow) Lower(value []RuleRow) C.RustBuffer {
	return LowerIntoRustBuffer[[]RuleRow](c, value)
}

func (c FfiConverterSequenceRuleRow) LowerExternal(value []RuleRow) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]RuleRow](c, value))
}

func (c FfiConverterSequenceRuleRow) Write(writer io.Writer, value []RuleRow) {
	if len(value) > math.MaxInt32 {
		panic("[]RuleRow is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterRuleRowINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceRuleRow struct{}

func (FfiDestroyerSequenceRuleRow) Destroy(sequence []RuleRow) {
	for _, value := range sequence {
		FfiDestroyerRuleRow{}.Destroy(value)
	}
}

type FfiConverterSequenceVerdictOption struct{}

var FfiConverterSequenceVerdictOptionINSTANCE = FfiConverterSequenceVerdictOption{}

func (c FfiConverterSequenceVerdictOption) Lift(rb RustBufferI) []VerdictOption {
	return LiftFromRustBuffer[[]VerdictOption](c, rb)
}

func (c FfiConverterSequenceVerdictOption) Read(reader io.Reader) []VerdictOption {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]VerdictOption, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterVerdictOptionINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceVerdictOption) Lower(value []VerdictOption) C.RustBuffer {
	return LowerIntoRustBuffer[[]VerdictOption](c, value)
}

func (c FfiConverterSequenceVerdictOption) LowerExternal(value []VerdictOption) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]VerdictOption](c, value))
}

func (c FfiConverterSequenceVerdictOption) Write(writer io.Writer, value []VerdictOption) {
	if len(value) > math.MaxInt32 {
		panic("[]VerdictOption is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterVerdictOptionINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceVerdictOption struct{}

func (FfiDestroyerSequenceVerdictOption) Destroy(sequence []VerdictOption) {
	for _, value := range sequence {
		FfiDestroyerVerdictOption{}.Destroy(value)
	}
}

type FfiConverterSequenceLocalizedText struct{}

var FfiConverterSequenceLocalizedTextINSTANCE = FfiConverterSequenceLocalizedText{}

func (c FfiConverterSequenceLocalizedText) Lift(rb RustBufferI) []fauna_core.LocalizedText {
	return LiftFromRustBuffer[[]fauna_core.LocalizedText](c, rb)
}

func (c FfiConverterSequenceLocalizedText) Read(reader io.Reader) []fauna_core.LocalizedText {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]fauna_core.LocalizedText, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceLocalizedText) Lower(value []fauna_core.LocalizedText) C.RustBuffer {
	return LowerIntoRustBuffer[[]fauna_core.LocalizedText](c, value)
}

func (c FfiConverterSequenceLocalizedText) LowerExternal(value []fauna_core.LocalizedText) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]fauna_core.LocalizedText](c, value))
}

func (c FfiConverterSequenceLocalizedText) Write(writer io.Writer, value []fauna_core.LocalizedText) {
	if len(value) > math.MaxInt32 {
		panic("[]fauna_core.LocalizedText is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceLocalizedText struct{}

func (FfiDestroyerSequenceLocalizedText) Destroy(sequence []fauna_core.LocalizedText) {
	for _, value := range sequence {
		fauna_core.FfiDestroyerLocalizedText{}.Destroy(value)
	}
}
