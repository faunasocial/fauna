package fauna_core

// #include <fauna_core.h>
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
		C.ffi_fauna_core_rustbuffer_free(cb.inner, status)
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
		return C.ffi_fauna_core_rustbuffer_from_bytes(foreign, status)
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
		return C.ffi_fauna_core_uniffi_contract_version()
	})
	if bindingsContractVersion != int(scaffoldingContractVersion) {
		// If this happens try cleaning and rebuilding your project
		panic("fauna_core: UniFFI contract version mismatch")
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_core_checksum_func_perimeter_mail_score_rows()
		})
		if checksum != 592 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_core: uniffi_fauna_core_checksum_func_perimeter_mail_score_rows: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_core_checksum_func_is_safe_payment_url()
		})
		if checksum != 18091 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_core: uniffi_fauna_core_checksum_func_is_safe_payment_url: UniFFI API checksum mismatch")
		}
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

type FfiConverterInt32 struct{}

var FfiConverterInt32INSTANCE = FfiConverterInt32{}

func (FfiConverterInt32) Lower(value int32) C.int32_t {
	return C.int32_t(value)
}

func (FfiConverterInt32) Write(writer io.Writer, value int32) {
	writeInt32(writer, value)
}

func (FfiConverterInt32) Lift(value C.int32_t) int32 {
	return int32(value)
}

func (FfiConverterInt32) Read(reader io.Reader) int32 {
	return readInt32(reader)
}

type FfiDestroyerInt32 struct{}

func (FfiDestroyerInt32) Destroy(_ int32) {}

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

// The text projection for one event-attendee row — the shared derivation behind
// the 6-client `AttendeeRow` (`docs/goal/ui/events.md` § Attendee list
// presentation). A CalDAV `ATTENDEE` is just `CN` + email + `PARTSTAT`, so this
// derives the three display strings every app renders — the CN→email
// fallback, the generated monogram initial, and the email-beneath visibility —
// in one place instead of each app hand-rolling them (they had drifted: some
// rendered the email twice when the CN *was* the email, one showed `?` for an
// email-only attendee, and whitespace handling varied). The RSVP status/color is
// deliberately *not* here — it stays a per-app idiomatic map, like
// [`crate::source_glyph`].
//
// Crosses the FFI/wasm boundary as-is (UniFFI `attendeeDisplay`, wasm
// `attendeeDisplay`), mirroring [`crate::format::RelativeTimeDisplay`].
type AttendeeDisplay struct {
	// Primary line: the `CN` when it is a real, distinct name, else the bare email.
	DisplayName string
	// Single uppercased initial of the display name for the generated monogram
	// avatar; `?` when there is no displayable character.
	Monogram string
	// The email shown beneath the name — `Some` only when the name is a real `CN`
	// distinct from the email (otherwise the email would appear twice), else `None`.
	SecondaryEmail *string
}

func (r *AttendeeDisplay) Destroy() {
	FfiDestroyerString{}.Destroy(r.DisplayName)
	FfiDestroyerString{}.Destroy(r.Monogram)
	FfiDestroyerOptionalString{}.Destroy(r.SecondaryEmail)
}

type FfiConverterAttendeeDisplay struct{}

var FfiConverterAttendeeDisplayINSTANCE = FfiConverterAttendeeDisplay{}

func (c FfiConverterAttendeeDisplay) Lift(rb RustBufferI) AttendeeDisplay {
	return LiftFromRustBuffer[AttendeeDisplay](c, rb)
}

func (c FfiConverterAttendeeDisplay) Read(reader io.Reader) AttendeeDisplay {
	return AttendeeDisplay{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterAttendeeDisplay) Lower(value AttendeeDisplay) C.RustBuffer {
	return LowerIntoRustBuffer[AttendeeDisplay](c, value)
}

func (c FfiConverterAttendeeDisplay) LowerExternal(value AttendeeDisplay) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AttendeeDisplay](c, value))
}

func (c FfiConverterAttendeeDisplay) Write(writer io.Writer, value AttendeeDisplay) {
	FfiConverterStringINSTANCE.Write(writer, value.DisplayName)
	FfiConverterStringINSTANCE.Write(writer, value.Monogram)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.SecondaryEmail)
}

type FfiDestroyerAttendeeDisplay struct{}

func (_ FfiDestroyerAttendeeDisplay) Destroy(value AttendeeDisplay) {
	value.Destroy()
}

// The four verdicts as one set.
//
// ⚠ `deny_unknown_fields` is carried over **verbatim** from both prior copies,
// which each had it; the unification is wire-neutral by construction
// and deliberately did not revisit it. Read it for what it is rather than as a
// ratified evolution posture: it means a *newer* sender that adds a fifth
// verdict field (a BIMI verdict, say) is **rejected outright** by an older
// reader, rather than having the unknown field ignored — which is the opposite
// of the additive-everywhere rule in
// `docs/goal/architecture/version-compatibility.md`. Nothing adds a field
// today, so nothing is broken today; whoever adds the fifth verdict must
// settle that first, and flipping the attribute is itself a wire-behavior
// change, not a cleanup. That warning lives here, on the type, so the author
// who adds the fifth verdict meets it at the definition they are editing.
//
// The `Default` derives are about *construction* churn, not wire compat: an
// all-`None` set is the RFC-canonical "no determination made" value, so
// fixtures pick it up with `..Default::default()`.
type AuthVerdicts struct {
	Dkim  DkimVerdict
	Spf   SpfVerdict
	Dmarc DmarcVerdict
	Arc   ArcVerdict
}

func (r *AuthVerdicts) Destroy() {
	FfiDestroyerDkimVerdict{}.Destroy(r.Dkim)
	FfiDestroyerSpfVerdict{}.Destroy(r.Spf)
	FfiDestroyerDmarcVerdict{}.Destroy(r.Dmarc)
	FfiDestroyerArcVerdict{}.Destroy(r.Arc)
}

type FfiConverterAuthVerdicts struct{}

var FfiConverterAuthVerdictsINSTANCE = FfiConverterAuthVerdicts{}

func (c FfiConverterAuthVerdicts) Lift(rb RustBufferI) AuthVerdicts {
	return LiftFromRustBuffer[AuthVerdicts](c, rb)
}

func (c FfiConverterAuthVerdicts) Read(reader io.Reader) AuthVerdicts {
	return AuthVerdicts{
		FfiConverterDkimVerdictINSTANCE.Read(reader),
		FfiConverterSpfVerdictINSTANCE.Read(reader),
		FfiConverterDmarcVerdictINSTANCE.Read(reader),
		FfiConverterArcVerdictINSTANCE.Read(reader),
	}
}

func (c FfiConverterAuthVerdicts) Lower(value AuthVerdicts) C.RustBuffer {
	return LowerIntoRustBuffer[AuthVerdicts](c, value)
}

func (c FfiConverterAuthVerdicts) LowerExternal(value AuthVerdicts) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AuthVerdicts](c, value))
}

func (c FfiConverterAuthVerdicts) Write(writer io.Writer, value AuthVerdicts) {
	FfiConverterDkimVerdictINSTANCE.Write(writer, value.Dkim)
	FfiConverterSpfVerdictINSTANCE.Write(writer, value.Spf)
	FfiConverterDmarcVerdictINSTANCE.Write(writer, value.Dmarc)
	FfiConverterArcVerdictINSTANCE.Write(writer, value.Arc)
}

type FfiDestroyerAuthVerdicts struct{}

func (_ FfiDestroyerAuthVerdicts) Destroy(value AuthVerdicts) {
	value.Destroy()
}

// One arm of `backup-destination-kind-select`: the wire discriminator the app
// records, and the text it paints.
//
// Crosses both boundaries directly (like [`BackupUsageDisplay`] below) rather
// than through an `Ffi*` mirror: `fauna_core` carries its own `uniffi` feature,
// so the mirror pattern the option catalogs in `fauna_client_admin` /
// `fauna_client_feed` need (`fauna_ffi::FfiRegistrationModeOption`) does not
// apply here.
type BackupDestinationKindOption struct {
	// The `BackupDestination.kind` value this option writes — never a per-app
	// string literal.
	Value string
	// The option text, which is deliberately the *same* [`LocalizedText`] the
	// row's badge will carry.
	Label LocalizedText
}

func (r *BackupDestinationKindOption) Destroy() {
	FfiDestroyerString{}.Destroy(r.Value)
	FfiDestroyerLocalizedText{}.Destroy(r.Label)
}

type FfiConverterBackupDestinationKindOption struct{}

var FfiConverterBackupDestinationKindOptionINSTANCE = FfiConverterBackupDestinationKindOption{}

func (c FfiConverterBackupDestinationKindOption) Lift(rb RustBufferI) BackupDestinationKindOption {
	return LiftFromRustBuffer[BackupDestinationKindOption](c, rb)
}

func (c FfiConverterBackupDestinationKindOption) Read(reader io.Reader) BackupDestinationKindOption {
	return BackupDestinationKindOption{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterBackupDestinationKindOption) Lower(value BackupDestinationKindOption) C.RustBuffer {
	return LowerIntoRustBuffer[BackupDestinationKindOption](c, value)
}

func (c FfiConverterBackupDestinationKindOption) LowerExternal(value BackupDestinationKindOption) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[BackupDestinationKindOption](c, value))
}

func (c FfiConverterBackupDestinationKindOption) Write(writer io.Writer, value BackupDestinationKindOption) {
	FfiConverterStringINSTANCE.Write(writer, value.Value)
	FfiConverterLocalizedTextINSTANCE.Write(writer, value.Label)
}

type FfiDestroyerBackupDestinationKindOption struct{}

func (_ FfiDestroyerBackupDestinationKindOption) Destroy(value BackupDestinationKindOption) {
	value.Destroy()
}

// The `backup-destination-last-audit-time` row text, split for client-side
// i18n exactly like [`BackupLastUploadDisplay`] — the same two-level shape
// because the same composition problem applies (a [`LocalizedText`] arg is a
// flat string, so the inner relative time must be localized client-side before
// substitution).
//
// It is a **separate type from the upload display on purpose**: the two rows
// answer different questions and are advanced by different parties. The upload
// row is the *source nest* reporting on its own work; this row is what the
// *client's own* audit could independently confirm. Collapsing them into one
// type would invite a client to render one where it meant the other — the
// precise confusion the audit exists to prevent.
type BackupLastAuditDisplay struct {
	Label LocalizedText
	When  *RelativeTimeDisplay
}

func (r *BackupLastAuditDisplay) Destroy() {
	FfiDestroyerLocalizedText{}.Destroy(r.Label)
	FfiDestroyerOptionalRelativeTimeDisplay{}.Destroy(r.When)
}

type FfiConverterBackupLastAuditDisplay struct{}

var FfiConverterBackupLastAuditDisplayINSTANCE = FfiConverterBackupLastAuditDisplay{}

func (c FfiConverterBackupLastAuditDisplay) Lift(rb RustBufferI) BackupLastAuditDisplay {
	return LiftFromRustBuffer[BackupLastAuditDisplay](c, rb)
}

func (c FfiConverterBackupLastAuditDisplay) Read(reader io.Reader) BackupLastAuditDisplay {
	return BackupLastAuditDisplay{
		FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterOptionalRelativeTimeDisplayINSTANCE.Read(reader),
	}
}

func (c FfiConverterBackupLastAuditDisplay) Lower(value BackupLastAuditDisplay) C.RustBuffer {
	return LowerIntoRustBuffer[BackupLastAuditDisplay](c, value)
}

func (c FfiConverterBackupLastAuditDisplay) LowerExternal(value BackupLastAuditDisplay) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[BackupLastAuditDisplay](c, value))
}

func (c FfiConverterBackupLastAuditDisplay) Write(writer io.Writer, value BackupLastAuditDisplay) {
	FfiConverterLocalizedTextINSTANCE.Write(writer, value.Label)
	FfiConverterOptionalRelativeTimeDisplayINSTANCE.Write(writer, value.When)
}

type FfiDestroyerBackupLastAuditDisplay struct{}

func (_ FfiDestroyerBackupLastAuditDisplay) Destroy(value BackupLastAuditDisplay) {
	value.Destroy()
}

// The `backup-destination-last-upload-time` row text, split for client-side
// i18n: `label` is the outer i18n key — `backups.backup_destination_last_upload_never`
// (complete as-is) when nothing has ever uploaded, else
// `backups.backup_destination_last_upload`, whose `{when}` placeholder the
// client fills by resolving `when` (a [`RelativeTimeDisplay`]; `Some` iff the
// label needs it) through its own pipeline first. Two levels because a
// [`LocalizedText`] arg is a flat string — the inner relative time must be
// localized client-side before substitution, exactly the composition every
// app hand-rolled. One source of truth for the never-vs-real decision,
// the unix-**seconds** → epoch-ms conversion, and the `> 0` guard (linux/web
// guarded zero timestamps; apple/android would have rendered the 1970 epoch —
// the richest shape wins). See backups.md § Per-destination status read and
// value-formatting.md § Backup destination status labels.
type BackupLastUploadDisplay struct {
	Label LocalizedText
	When  *RelativeTimeDisplay
}

func (r *BackupLastUploadDisplay) Destroy() {
	FfiDestroyerLocalizedText{}.Destroy(r.Label)
	FfiDestroyerOptionalRelativeTimeDisplay{}.Destroy(r.When)
}

type FfiConverterBackupLastUploadDisplay struct{}

var FfiConverterBackupLastUploadDisplayINSTANCE = FfiConverterBackupLastUploadDisplay{}

func (c FfiConverterBackupLastUploadDisplay) Lift(rb RustBufferI) BackupLastUploadDisplay {
	return LiftFromRustBuffer[BackupLastUploadDisplay](c, rb)
}

func (c FfiConverterBackupLastUploadDisplay) Read(reader io.Reader) BackupLastUploadDisplay {
	return BackupLastUploadDisplay{
		FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterOptionalRelativeTimeDisplayINSTANCE.Read(reader),
	}
}

func (c FfiConverterBackupLastUploadDisplay) Lower(value BackupLastUploadDisplay) C.RustBuffer {
	return LowerIntoRustBuffer[BackupLastUploadDisplay](c, value)
}

func (c FfiConverterBackupLastUploadDisplay) LowerExternal(value BackupLastUploadDisplay) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[BackupLastUploadDisplay](c, value))
}

func (c FfiConverterBackupLastUploadDisplay) Write(writer io.Writer, value BackupLastUploadDisplay) {
	FfiConverterLocalizedTextINSTANCE.Write(writer, value.Label)
	FfiConverterOptionalRelativeTimeDisplayINSTANCE.Write(writer, value.When)
}

type FfiDestroyerBackupLastUploadDisplay struct{}

func (_ FfiDestroyerBackupLastUploadDisplay) Destroy(value BackupLastUploadDisplay) {
	value.Destroy()
}

// The `backup-destination-usage` row text — client-device rows only: held bytes
// against the user-set cap.
//
// Split for client-side i18n exactly like [`BackupLastUploadDisplay`], and for
// the same reason: a [`LocalizedText`] arg is a flat string, so the two inner
// [`byte_size`] texts must be localized client-side before substitution.
//
// **Cap-reached is read from `cap_state`, never inferred from `held >= cap`.**
// The pull pass reports `CAP_STATE_REACHED` for a pass that stopped at its cap
// even though it ends *below* the cap (a segment larger than the remaining
// headroom stops the pass without filling it) — so a client re-deriving the
// verdict from the two numbers would render "healthy, with room to spare" for a
// backup that has silently stopped advancing. That inversion is
// mutation-pinned in `fauna-sync-engine`'s pull tests; this label is the other
// half of the same guarantee.
//
// See `docs/goal/behavior/backup-destinations.md` § Third destination kind and
// `docs/goal/behavior/value-formatting.md` § Backup destination status labels.
type BackupUsageDisplay struct {
	// The outer key. Carries `{held}` and/or `{cap}` placeholders the client
	// fills from the resolved fields below.
	Label LocalizedText
	// The held-bytes text to resolve and substitute for `{held}`; `Some` iff
	// `label` names it.
	Held *LocalizedText
	// The cap text to resolve and substitute for `{cap}`; `Some` iff `label`
	// names it.
	Cap *LocalizedText
}

func (r *BackupUsageDisplay) Destroy() {
	FfiDestroyerLocalizedText{}.Destroy(r.Label)
	FfiDestroyerOptionalLocalizedText{}.Destroy(r.Held)
	FfiDestroyerOptionalLocalizedText{}.Destroy(r.Cap)
}

type FfiConverterBackupUsageDisplay struct{}

var FfiConverterBackupUsageDisplayINSTANCE = FfiConverterBackupUsageDisplay{}

func (c FfiConverterBackupUsageDisplay) Lift(rb RustBufferI) BackupUsageDisplay {
	return LiftFromRustBuffer[BackupUsageDisplay](c, rb)
}

func (c FfiConverterBackupUsageDisplay) Read(reader io.Reader) BackupUsageDisplay {
	return BackupUsageDisplay{
		FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterOptionalLocalizedTextINSTANCE.Read(reader),
		FfiConverterOptionalLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterBackupUsageDisplay) Lower(value BackupUsageDisplay) C.RustBuffer {
	return LowerIntoRustBuffer[BackupUsageDisplay](c, value)
}

func (c FfiConverterBackupUsageDisplay) LowerExternal(value BackupUsageDisplay) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[BackupUsageDisplay](c, value))
}

func (c FfiConverterBackupUsageDisplay) Write(writer io.Writer, value BackupUsageDisplay) {
	FfiConverterLocalizedTextINSTANCE.Write(writer, value.Label)
	FfiConverterOptionalLocalizedTextINSTANCE.Write(writer, value.Held)
	FfiConverterOptionalLocalizedTextINSTANCE.Write(writer, value.Cap)
}

type FfiDestroyerBackupUsageDisplay struct{}

func (_ FfiDestroyerBackupUsageDisplay) Destroy(value BackupUsageDisplay) {
	value.Destroy()
}

// Who a bridge is, as a thread row or a feed badge paints it — the identity a
// bridge principal declares in its manifest's `bridge` block
// (`architecture/third-party.md` § The manifest → *The `bridge` block*),
// reduced to the three fields an app renders. Carried on
// `fauna_conversations::ThreadSummary::bridge` / `ThreadDetail::bridge` for a
// `Rail::Bridged` thread (`ui/conversations.md` § Where logic lives → *The
// `Bridged` adapter*, ruling 2 (a)) and handed to
// `fauna_feed::classify_sources` as the roster a bridged post's source token
// resolves against (`ui/feed.md` § Implementation status today) — one type for
// both, so the conversation row and the feed badge cannot name a bridge
// differently.
type BridgeIdentitySnapshot struct {
	// The manifest's `bridge.id` — a one-label lowercase name, unique per
	// account roster; the source token a bridged post carries.
	Id string
	// The bridge's declared display label (`Matrix`, …).
	Label string
	// The declared glyph — one member of the fixed set; never an image.
	Glyph SourceGlyph
}

func (r *BridgeIdentitySnapshot) Destroy() {
	FfiDestroyerString{}.Destroy(r.Id)
	FfiDestroyerString{}.Destroy(r.Label)
	FfiDestroyerSourceGlyph{}.Destroy(r.Glyph)
}

type FfiConverterBridgeIdentitySnapshot struct{}

var FfiConverterBridgeIdentitySnapshotINSTANCE = FfiConverterBridgeIdentitySnapshot{}

func (c FfiConverterBridgeIdentitySnapshot) Lift(rb RustBufferI) BridgeIdentitySnapshot {
	return LiftFromRustBuffer[BridgeIdentitySnapshot](c, rb)
}

func (c FfiConverterBridgeIdentitySnapshot) Read(reader io.Reader) BridgeIdentitySnapshot {
	return BridgeIdentitySnapshot{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterSourceGlyphINSTANCE.Read(reader),
	}
}

func (c FfiConverterBridgeIdentitySnapshot) Lower(value BridgeIdentitySnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[BridgeIdentitySnapshot](c, value)
}

func (c FfiConverterBridgeIdentitySnapshot) LowerExternal(value BridgeIdentitySnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[BridgeIdentitySnapshot](c, value))
}

func (c FfiConverterBridgeIdentitySnapshot) Write(writer io.Writer, value BridgeIdentitySnapshot) {
	FfiConverterStringINSTANCE.Write(writer, value.Id)
	FfiConverterStringINSTANCE.Write(writer, value.Label)
	FfiConverterSourceGlyphINSTANCE.Write(writer, value.Glyph)
}

type FfiDestroyerBridgeIdentitySnapshot struct{}

func (_ FfiDestroyerBridgeIdentitySnapshot) Destroy(value BridgeIdentitySnapshot) {
	value.Destroy()
}

// The whole `admin-dns` served-cert badge, decided once: the state word plus
// **which** of the two mutually-exclusive sub-labels follows it. Clients render
// `"{admin.dns.cert.label} {state}"`, then append `({admin.dns.cert.self_signed})`
// when `show_self_signed`, or `admin.dns.cert.expires{date}` when
// `expires_at_unix` is `Some` — formatting that epoch with the platform's native,
// locale-aware date formatter, the same split as [`RelativeTimeDisplay`].
//
// The two are never both set: a self-signed floor cert's own expiry is not the
// admin's concern — what it needs is a *trusted* cert — so the date is withheld
// and the badge says `self-signed` instead. This is the decision the per-app
// copies drifted on, and the reason it is no longer theirs to make: the floor
// cert carries a genuine, far-future `not_after` (rcgen's default 4096-01-01;
// `bins/fauna-nest/src/acme.rs` mints it with no override), so testing `is_floor`
// and `not_after_unix > 0` *independently* — as apple's `AdminDnsView` did —
// renders "Certificate: Renew needed (self-signed) — expires 4096-01-01" on
// every TLS-serving fresh nest: a two-millennium expiry reading as reassurance
// on an untrusted cert.
//
// value-formatting.md § Cert status badge; tls-certificates.md § C.4 (the row's
// three states) and § Where logic lives ("per-app shells … no cert logic of
// their own"). The badge's colour stays an idiomatic per-app render, the same
// split as [`cert_status_label`] and [`contact_status_label`].
type CertStatusView struct {
	// The state word — [`cert_status_label`]'s key, so one call resolves the
	// whole badge and no client re-derives the state → key map.
	State LocalizedText
	// Append the `(self-signed)` sub-label — the cert is the floor.
	ShowSelfSigned bool
	// `Some(epoch)` iff the expiry date belongs on the badge: a trusted cert
	// (valid or expiring) whose `not_after_unix` the nest actually reported.
	// `None` for the floor (see above) and for the `0` a nest with no TLS
	// resolver at all sends.
	ExpiresAtUnix *int64
}

func (r *CertStatusView) Destroy() {
	FfiDestroyerLocalizedText{}.Destroy(r.State)
	FfiDestroyerBool{}.Destroy(r.ShowSelfSigned)
	FfiDestroyerOptionalInt64{}.Destroy(r.ExpiresAtUnix)
}

type FfiConverterCertStatusView struct{}

var FfiConverterCertStatusViewINSTANCE = FfiConverterCertStatusView{}

func (c FfiConverterCertStatusView) Lift(rb RustBufferI) CertStatusView {
	return LiftFromRustBuffer[CertStatusView](c, rb)
}

func (c FfiConverterCertStatusView) Read(reader io.Reader) CertStatusView {
	return CertStatusView{
		FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterOptionalInt64INSTANCE.Read(reader),
	}
}

func (c FfiConverterCertStatusView) Lower(value CertStatusView) C.RustBuffer {
	return LowerIntoRustBuffer[CertStatusView](c, value)
}

func (c FfiConverterCertStatusView) LowerExternal(value CertStatusView) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[CertStatusView](c, value))
}

func (c FfiConverterCertStatusView) Write(writer io.Writer, value CertStatusView) {
	FfiConverterLocalizedTextINSTANCE.Write(writer, value.State)
	FfiConverterBoolINSTANCE.Write(writer, value.ShowSelfSigned)
	FfiConverterOptionalInt64INSTANCE.Write(writer, value.ExpiresAtUnix)
}

type FfiDestroyerCertStatusView struct{}

func (_ FfiDestroyerCertStatusView) Destroy(value CertStatusView) {
	value.Destroy()
}

// The per-row content-label data unit (`moderation.md` § Per-row badge data
// path, ratified 2026-07-16) — one classifier verdict on one content item.
// `confidence_per_mille` is the 0–1000 scaling of the classifier's 0.0–1.0
// confidence (the dag-cbor wire forbids floats — the established convention,
// e.g. [`crate::format::confidence_percent`]). Rides directly on the wire
// embedded in `fauna_protocol::feed::FeedPostItem.labels` /
// `fauna_client_conversations::MessageSnapshot.labels` (no protocol-local
// mirror needed — this type is already float-free). `category` is one of the
// canonical 5 (or an off-list producer string, which
// [`content_label_style`] degrades to the grey `Other` badge).
type ContentLabelEntry struct {
	Category           string
	ConfidencePerMille uint16
}

func (r *ContentLabelEntry) Destroy() {
	FfiDestroyerString{}.Destroy(r.Category)
	FfiDestroyerUint16{}.Destroy(r.ConfidencePerMille)
}

type FfiConverterContentLabelEntry struct{}

var FfiConverterContentLabelEntryINSTANCE = FfiConverterContentLabelEntry{}

func (c FfiConverterContentLabelEntry) Lift(rb RustBufferI) ContentLabelEntry {
	return LiftFromRustBuffer[ContentLabelEntry](c, rb)
}

func (c FfiConverterContentLabelEntry) Read(reader io.Reader) ContentLabelEntry {
	return ContentLabelEntry{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUint16INSTANCE.Read(reader),
	}
}

func (c FfiConverterContentLabelEntry) Lower(value ContentLabelEntry) C.RustBuffer {
	return LowerIntoRustBuffer[ContentLabelEntry](c, value)
}

func (c FfiConverterContentLabelEntry) LowerExternal(value ContentLabelEntry) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ContentLabelEntry](c, value))
}

func (c FfiConverterContentLabelEntry) Write(writer io.Writer, value ContentLabelEntry) {
	FfiConverterStringINSTANCE.Write(writer, value.Category)
	FfiConverterUint16INSTANCE.Write(writer, value.ConfidencePerMille)
}

type FfiDestroyerContentLabelEntry struct{}

func (_ FfiDestroyerContentLabelEntry) Destroy(value ContentLabelEntry) {
	value.Destroy()
}

// Shared presentation of a content-label badge: the localized `label`, an emoji
// `icon`, and the two-tone colour (`tint` background base + higher-contrast
// `accent` for text/icon), all hex. Each app maps the hex strings to its
// native colour type and resolves `label.key` through its own i18n pipeline; no
// client hard-codes the category→style map (moderation.md § Where logic lives —
// the drift #157 lift).
type ContentLabelStyle struct {
	Label  LocalizedText
	Icon   string
	Tint   string
	Accent string
}

func (r *ContentLabelStyle) Destroy() {
	FfiDestroyerLocalizedText{}.Destroy(r.Label)
	FfiDestroyerString{}.Destroy(r.Icon)
	FfiDestroyerString{}.Destroy(r.Tint)
	FfiDestroyerString{}.Destroy(r.Accent)
}

type FfiConverterContentLabelStyle struct{}

var FfiConverterContentLabelStyleINSTANCE = FfiConverterContentLabelStyle{}

func (c FfiConverterContentLabelStyle) Lift(rb RustBufferI) ContentLabelStyle {
	return LiftFromRustBuffer[ContentLabelStyle](c, rb)
}

func (c FfiConverterContentLabelStyle) Read(reader io.Reader) ContentLabelStyle {
	return ContentLabelStyle{
		FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterContentLabelStyle) Lower(value ContentLabelStyle) C.RustBuffer {
	return LowerIntoRustBuffer[ContentLabelStyle](c, value)
}

func (c FfiConverterContentLabelStyle) LowerExternal(value ContentLabelStyle) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ContentLabelStyle](c, value))
}

func (c FfiConverterContentLabelStyle) Write(writer io.Writer, value ContentLabelStyle) {
	FfiConverterLocalizedTextINSTANCE.Write(writer, value.Label)
	FfiConverterStringINSTANCE.Write(writer, value.Icon)
	FfiConverterStringINSTANCE.Write(writer, value.Tint)
	FfiConverterStringINSTANCE.Write(writer, value.Accent)
}

type FfiDestroyerContentLabelStyle struct{}

func (_ FfiDestroyerContentLabelStyle) Destroy(value ContentLabelStyle) {
	value.Destroy()
}

// Boundary-friendly flattening of [`conversation_timestamp`] for the FFI / wasm
// clients: exactly one field is `Some`. `clock` (`"HH:MM"`, 24 h, local) for
// today; `localized` for Yesterday / a weekday name; `absolute_epoch_ms` for
// older items (the client formats a locale-aware date, as with
// [`RelativeTimeDisplay`]). Native apps can use the
// [`ConversationTimestamp`] enum directly (e.g. a locale-aware 12 h clock off
// `Today { hour, minute }`).
type ConversationTimestampDisplay struct {
	Clock           *string
	Localized       *LocalizedText
	AbsoluteEpochMs *int64
}

func (r *ConversationTimestampDisplay) Destroy() {
	FfiDestroyerOptionalString{}.Destroy(r.Clock)
	FfiDestroyerOptionalLocalizedText{}.Destroy(r.Localized)
	FfiDestroyerOptionalInt64{}.Destroy(r.AbsoluteEpochMs)
}

type FfiConverterConversationTimestampDisplay struct{}

var FfiConverterConversationTimestampDisplayINSTANCE = FfiConverterConversationTimestampDisplay{}

func (c FfiConverterConversationTimestampDisplay) Lift(rb RustBufferI) ConversationTimestampDisplay {
	return LiftFromRustBuffer[ConversationTimestampDisplay](c, rb)
}

func (c FfiConverterConversationTimestampDisplay) Read(reader io.Reader) ConversationTimestampDisplay {
	return ConversationTimestampDisplay{
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalLocalizedTextINSTANCE.Read(reader),
		FfiConverterOptionalInt64INSTANCE.Read(reader),
	}
}

func (c FfiConverterConversationTimestampDisplay) Lower(value ConversationTimestampDisplay) C.RustBuffer {
	return LowerIntoRustBuffer[ConversationTimestampDisplay](c, value)
}

func (c FfiConverterConversationTimestampDisplay) LowerExternal(value ConversationTimestampDisplay) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ConversationTimestampDisplay](c, value))
}

func (c FfiConverterConversationTimestampDisplay) Write(writer io.Writer, value ConversationTimestampDisplay) {
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Clock)
	FfiConverterOptionalLocalizedTextINSTANCE.Write(writer, value.Localized)
	FfiConverterOptionalInt64INSTANCE.Write(writer, value.AbsoluteEpochMs)
}

type FfiDestroyerConversationTimestampDisplay struct{}

func (_ FfiDestroyerConversationTimestampDisplay) Destroy(value ConversationTimestampDisplay) {
	value.Destroy()
}

// Owned `(url, state name)` pair from [`RenderDocument::link_previews`], for the
// FFI/wasm face every non-Rust app's e2e state dump publishes as
// `data.feed.posts[].link_previews` (`{url, state}`, `state` being
// [`PreviewState::name`]). A plain `{url, state}` record so each app serializes it
// as-is — the same shape the Rust apps build from the borrowed pairs.
type LinkPreviewStateOwned struct {
	Url   string
	State string
}

func (r *LinkPreviewStateOwned) Destroy() {
	FfiDestroyerString{}.Destroy(r.Url)
	FfiDestroyerString{}.Destroy(r.State)
}

type FfiConverterLinkPreviewStateOwned struct{}

var FfiConverterLinkPreviewStateOwnedINSTANCE = FfiConverterLinkPreviewStateOwned{}

func (c FfiConverterLinkPreviewStateOwned) Lift(rb RustBufferI) LinkPreviewStateOwned {
	return LiftFromRustBuffer[LinkPreviewStateOwned](c, rb)
}

func (c FfiConverterLinkPreviewStateOwned) Read(reader io.Reader) LinkPreviewStateOwned {
	return LinkPreviewStateOwned{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterLinkPreviewStateOwned) Lower(value LinkPreviewStateOwned) C.RustBuffer {
	return LowerIntoRustBuffer[LinkPreviewStateOwned](c, value)
}

func (c FfiConverterLinkPreviewStateOwned) LowerExternal(value LinkPreviewStateOwned) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[LinkPreviewStateOwned](c, value))
}

func (c FfiConverterLinkPreviewStateOwned) Write(writer io.Writer, value LinkPreviewStateOwned) {
	FfiConverterStringINSTANCE.Write(writer, value.Url)
	FfiConverterStringINSTANCE.Write(writer, value.State)
}

type FfiDestroyerLinkPreviewStateOwned struct{}

func (_ FfiDestroyerLinkPreviewStateOwned) Destroy(value LinkPreviewStateOwned) {
	value.Destroy()
}

// An i18n key plus a flat substitution map. `key` is the string key from
// `i18n/strings/en.yaml`; `args` is `placeholder name → value`. Clients pass
// `(key, args)` through their platform's localization pipeline. An empty key
// means "no text".
type LocalizedText struct {
	Key  string
	Args map[string]string
}

func (r *LocalizedText) Destroy() {
	FfiDestroyerString{}.Destroy(r.Key)
	FfiDestroyerMapStringString{}.Destroy(r.Args)
}

type FfiConverterLocalizedText struct{}

var FfiConverterLocalizedTextINSTANCE = FfiConverterLocalizedText{}

func (c FfiConverterLocalizedText) Lift(rb RustBufferI) LocalizedText {
	return LiftFromRustBuffer[LocalizedText](c, rb)
}

func (c FfiConverterLocalizedText) Read(reader io.Reader) LocalizedText {
	return LocalizedText{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterMapStringStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterLocalizedText) Lower(value LocalizedText) C.RustBuffer {
	return LowerIntoRustBuffer[LocalizedText](c, value)
}

func (c FfiConverterLocalizedText) LowerExternal(value LocalizedText) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[LocalizedText](c, value))
}

func (c FfiConverterLocalizedText) Write(writer io.Writer, value LocalizedText) {
	FfiConverterStringINSTANCE.Write(writer, value.Key)
	FfiConverterMapStringStringINSTANCE.Write(writer, value.Args)
}

type FfiDestroyerLocalizedText struct{}

func (_ FfiDestroyerLocalizedText) Destroy(value LocalizedText) {
	value.Destroy()
}

// The text parts of one review row, for every surface that renders one.
//
// Deliberately *parts* rather than one finished sentence. The row reads
// `{who} — {reason}` (`settings.recovery_kit.review_row`) and both halves are
// translatable, but `who` is frequently a user-supplied handle:
// [`crate::localized::LocalizedText::resolve_args`] would translate a handle
// that happened to equal an i18n key — a hazard its own documentation calls
// out — so composition is left to the caller, which resolves each part with
// plain [`crate::localized::LocalizedText::resolve`] and joins the reasons.
// That also lets the join survive a person carrying several reasons, which a
// single template argument cannot express.
//
// It exists for the reason [`is_under_review`] does, one level up: the rules
// encoded here are subtle, their failure mode is a row that silently says
// *less* than it should, and seven renderers re-deriving them is seven
// chances to get one wrong.
type MemberReviewRowText struct {
	// Who the row is about — the handle verbatim when one resolves, and
	// otherwise the "no longer in any of your groups" key. A person the app
	// can no longer name is still a person the owner has to decide about, so
	// the row is named rather than dropped.
	Who LocalizedText
	// One entry per distinct reason, in first-raised order. Never empty for a
	// review that reached a surface, and **an unrecognised reason is carried,
	// not filtered**: hiding the row would be exactly the silent
	// disappearance the fail-visible rule exists to prevent.
	Reasons []LocalizedText
}

func (r *MemberReviewRowText) Destroy() {
	FfiDestroyerLocalizedText{}.Destroy(r.Who)
	FfiDestroyerSequenceLocalizedText{}.Destroy(r.Reasons)
}

type FfiConverterMemberReviewRowText struct{}

var FfiConverterMemberReviewRowTextINSTANCE = FfiConverterMemberReviewRowText{}

func (c FfiConverterMemberReviewRowText) Lift(rb RustBufferI) MemberReviewRowText {
	return LiftFromRustBuffer[MemberReviewRowText](c, rb)
}

func (c FfiConverterMemberReviewRowText) Read(reader io.Reader) MemberReviewRowText {
	return MemberReviewRowText{
		FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterSequenceLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterMemberReviewRowText) Lower(value MemberReviewRowText) C.RustBuffer {
	return LowerIntoRustBuffer[MemberReviewRowText](c, value)
}

func (c FfiConverterMemberReviewRowText) LowerExternal(value MemberReviewRowText) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[MemberReviewRowText](c, value))
}

func (c FfiConverterMemberReviewRowText) Write(writer io.Writer, value MemberReviewRowText) {
	FfiConverterLocalizedTextINSTANCE.Write(writer, value.Who)
	FfiConverterSequenceLocalizedTextINSTANCE.Write(writer, value.Reasons)
}

type FfiDestroyerMemberReviewRowText struct{}

func (_ FfiDestroyerMemberReviewRowText) Destroy(value MemberReviewRowText) {
	value.Destroy()
}

// One entry of the user's muted-keywords list: the term and how hard it
// demotes (`content-moderation-and-ranking.md` § Composition, the 2026-07-10
// ruling — "each muted-keywords entry is `(keyword, weight)`").
type MutedKeyword struct {
	// The term, case preserved for display; matched case-insensitively as a
	// substring (`crate::keyword::body_excludes_matches`).
	Keyword string
	// The factor value a match contributes, per-mille, in
	// `[MUTED_KEYWORDS_PENALTY, 0]` (`crate::scoring::clamp_muted_keyword_weight`
	// reads any other value as its nearest bound). The default,
	// `MUTED_KEYWORDS_PENALTY` (−1000), sinks the item and collapses it; a
	// softer weight only demotes it in a ranked feed.
	Weight int64
}

func (r *MutedKeyword) Destroy() {
	FfiDestroyerString{}.Destroy(r.Keyword)
	FfiDestroyerInt64{}.Destroy(r.Weight)
}

type FfiConverterMutedKeyword struct{}

var FfiConverterMutedKeywordINSTANCE = FfiConverterMutedKeyword{}

func (c FfiConverterMutedKeyword) Lift(rb RustBufferI) MutedKeyword {
	return LiftFromRustBuffer[MutedKeyword](c, rb)
}

func (c FfiConverterMutedKeyword) Read(reader io.Reader) MutedKeyword {
	return MutedKeyword{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
	}
}

func (c FfiConverterMutedKeyword) Lower(value MutedKeyword) C.RustBuffer {
	return LowerIntoRustBuffer[MutedKeyword](c, value)
}

func (c FfiConverterMutedKeyword) LowerExternal(value MutedKeyword) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[MutedKeyword](c, value))
}

func (c FfiConverterMutedKeyword) Write(writer io.Writer, value MutedKeyword) {
	FfiConverterStringINSTANCE.Write(writer, value.Keyword)
	FfiConverterInt64INSTANCE.Write(writer, value.Weight)
}

type FfiDestroyerMutedKeyword struct{}

func (_ FfiDestroyerMutedKeyword) Destroy(value MutedKeyword) {
	value.Destroy()
}

// [`orphaned_store_label`]'s two-level result — the row sentence plus the
// byte size that fills its `{held}` slot, each a [`LocalizedText`] so the unit
// resolves in the reader's language before substitution.
type OrphanedStoreDisplay struct {
	// The row sentence, carrying `{held}`.
	Label LocalizedText
	// The size that fills it.
	Held LocalizedText
}

func (r *OrphanedStoreDisplay) Destroy() {
	FfiDestroyerLocalizedText{}.Destroy(r.Label)
	FfiDestroyerLocalizedText{}.Destroy(r.Held)
}

type FfiConverterOrphanedStoreDisplay struct{}

var FfiConverterOrphanedStoreDisplayINSTANCE = FfiConverterOrphanedStoreDisplay{}

func (c FfiConverterOrphanedStoreDisplay) Lift(rb RustBufferI) OrphanedStoreDisplay {
	return LiftFromRustBuffer[OrphanedStoreDisplay](c, rb)
}

func (c FfiConverterOrphanedStoreDisplay) Read(reader io.Reader) OrphanedStoreDisplay {
	return OrphanedStoreDisplay{
		FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterOrphanedStoreDisplay) Lower(value OrphanedStoreDisplay) C.RustBuffer {
	return LowerIntoRustBuffer[OrphanedStoreDisplay](c, value)
}

func (c FfiConverterOrphanedStoreDisplay) LowerExternal(value OrphanedStoreDisplay) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[OrphanedStoreDisplay](c, value))
}

func (c FfiConverterOrphanedStoreDisplay) Write(writer io.Writer, value OrphanedStoreDisplay) {
	FfiConverterLocalizedTextINSTANCE.Write(writer, value.Label)
	FfiConverterLocalizedTextINSTANCE.Write(writer, value.Held)
}

type FfiDestroyerOrphanedStoreDisplay struct{}

func (_ FfiDestroyerOrphanedStoreDisplay) Destroy(value OrphanedStoreDisplay) {
	value.Destroy()
}

// The private section's staged form: plain text as the user sees it.
type OverlayForm struct {
	Nickname string
	Notes    string
	// Display spellings, in the order shown.
	Labels []string
}

func (r *OverlayForm) Destroy() {
	FfiDestroyerString{}.Destroy(r.Nickname)
	FfiDestroyerString{}.Destroy(r.Notes)
	FfiDestroyerSequenceString{}.Destroy(r.Labels)
}

type FfiConverterOverlayForm struct{}

var FfiConverterOverlayFormINSTANCE = FfiConverterOverlayForm{}

func (c FfiConverterOverlayForm) Lift(rb RustBufferI) OverlayForm {
	return LiftFromRustBuffer[OverlayForm](c, rb)
}

func (c FfiConverterOverlayForm) Read(reader io.Reader) OverlayForm {
	return OverlayForm{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterOverlayForm) Lower(value OverlayForm) C.RustBuffer {
	return LowerIntoRustBuffer[OverlayForm](c, value)
}

func (c FfiConverterOverlayForm) LowerExternal(value OverlayForm) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[OverlayForm](c, value))
}

func (c FfiConverterOverlayForm) Write(writer io.Writer, value OverlayForm) {
	FfiConverterStringINSTANCE.Write(writer, value.Nickname)
	FfiConverterStringINSTANCE.Write(writer, value.Notes)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.Labels)
}

type FfiDestroyerOverlayForm struct{}

func (_ FfiDestroyerOverlayForm) Destroy(value OverlayForm) {
	value.Destroy()
}

// What to call another person — [`peer_display_label`]'s answer.
type PeerLabel struct {
	// The one line every surface shows.
	Primary string
	// `Some` only when the viewer's own nickname supplied [`Self::primary`]:
	// what `primary` would have been without it, for the secondary line a
	// list or detail surface renders so a private name never hides the
	// public identity.
	Public *string
}

func (r *PeerLabel) Destroy() {
	FfiDestroyerString{}.Destroy(r.Primary)
	FfiDestroyerOptionalString{}.Destroy(r.Public)
}

type FfiConverterPeerLabel struct{}

var FfiConverterPeerLabelINSTANCE = FfiConverterPeerLabel{}

func (c FfiConverterPeerLabel) Lift(rb RustBufferI) PeerLabel {
	return LiftFromRustBuffer[PeerLabel](c, rb)
}

func (c FfiConverterPeerLabel) Read(reader io.Reader) PeerLabel {
	return PeerLabel{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterPeerLabel) Lower(value PeerLabel) C.RustBuffer {
	return LowerIntoRustBuffer[PeerLabel](c, value)
}

func (c FfiConverterPeerLabel) LowerExternal(value PeerLabel) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[PeerLabel](c, value))
}

func (c FfiConverterPeerLabel) Write(writer io.Writer, value PeerLabel) {
	FfiConverterStringINSTANCE.Write(writer, value.Primary)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Public)
}

type FfiDestroyerPeerLabel struct{}

func (_ FfiDestroyerPeerLabel) Destroy(value PeerLabel) {
	value.Destroy()
}

// One rendered line of the read-only policy summary the *supervised* side sees
// (`family-policy-summary`). Structured rather than a pre-joined string so each
// app keeps its own line layout — linux joins with `\n`, the mobile clients
// stack rows.
type PolicySummaryLine struct {
	// The knob's name (`family.policy_*_label`).
	Label LocalizedText
	// The knob's current value, already fail-closed for the two string knobs.
	Value LocalizedText
}

func (r *PolicySummaryLine) Destroy() {
	FfiDestroyerLocalizedText{}.Destroy(r.Label)
	FfiDestroyerLocalizedText{}.Destroy(r.Value)
}

type FfiConverterPolicySummaryLine struct{}

var FfiConverterPolicySummaryLineINSTANCE = FfiConverterPolicySummaryLine{}

func (c FfiConverterPolicySummaryLine) Lift(rb RustBufferI) PolicySummaryLine {
	return LiftFromRustBuffer[PolicySummaryLine](c, rb)
}

func (c FfiConverterPolicySummaryLine) Read(reader io.Reader) PolicySummaryLine {
	return PolicySummaryLine{
		FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterPolicySummaryLine) Lower(value PolicySummaryLine) C.RustBuffer {
	return LowerIntoRustBuffer[PolicySummaryLine](c, value)
}

func (c FfiConverterPolicySummaryLine) LowerExternal(value PolicySummaryLine) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[PolicySummaryLine](c, value))
}

func (c FfiConverterPolicySummaryLine) Write(writer io.Writer, value PolicySummaryLine) {
	FfiConverterLocalizedTextINSTANCE.Write(writer, value.Label)
	FfiConverterLocalizedTextINSTANCE.Write(writer, value.Value)
}

type FfiDestroyerPolicySummaryLine struct{}

func (_ FfiDestroyerPolicySummaryLine) Destroy(value PolicySummaryLine) {
	value.Destroy()
}

// Owned mirror of [`ProxiedImageRef`] — same cross-boundary reason as
// [`QuotedPostEmbedOwned`], for the FFI/wasm face of [`RenderDocument::proxied_images`].
type ProxiedImageRefOwned struct {
	Path string
	Alt  string
}

func (r *ProxiedImageRefOwned) Destroy() {
	FfiDestroyerString{}.Destroy(r.Path)
	FfiDestroyerString{}.Destroy(r.Alt)
}

type FfiConverterProxiedImageRefOwned struct{}

var FfiConverterProxiedImageRefOwnedINSTANCE = FfiConverterProxiedImageRefOwned{}

func (c FfiConverterProxiedImageRefOwned) Lift(rb RustBufferI) ProxiedImageRefOwned {
	return LiftFromRustBuffer[ProxiedImageRefOwned](c, rb)
}

func (c FfiConverterProxiedImageRefOwned) Read(reader io.Reader) ProxiedImageRefOwned {
	return ProxiedImageRefOwned{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterProxiedImageRefOwned) Lower(value ProxiedImageRefOwned) C.RustBuffer {
	return LowerIntoRustBuffer[ProxiedImageRefOwned](c, value)
}

func (c FfiConverterProxiedImageRefOwned) LowerExternal(value ProxiedImageRefOwned) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ProxiedImageRefOwned](c, value))
}

func (c FfiConverterProxiedImageRefOwned) Write(writer io.Writer, value ProxiedImageRefOwned) {
	FfiConverterStringINSTANCE.Write(writer, value.Path)
	FfiConverterStringINSTANCE.Write(writer, value.Alt)
}

type FfiDestroyerProxiedImageRefOwned struct{}

func (_ FfiDestroyerProxiedImageRefOwned) Destroy(value ProxiedImageRefOwned) {
	value.Destroy()
}

// Owned mirror of [`ProxiedVideoRef`] — same cross-boundary reason as
// [`QuotedPostEmbedOwned`], for the FFI/wasm face of [`RenderDocument::proxied_videos`].
type ProxiedVideoRefOwned struct {
	Path string
	Alt  string
}

func (r *ProxiedVideoRefOwned) Destroy() {
	FfiDestroyerString{}.Destroy(r.Path)
	FfiDestroyerString{}.Destroy(r.Alt)
}

type FfiConverterProxiedVideoRefOwned struct{}

var FfiConverterProxiedVideoRefOwnedINSTANCE = FfiConverterProxiedVideoRefOwned{}

func (c FfiConverterProxiedVideoRefOwned) Lift(rb RustBufferI) ProxiedVideoRefOwned {
	return LiftFromRustBuffer[ProxiedVideoRefOwned](c, rb)
}

func (c FfiConverterProxiedVideoRefOwned) Read(reader io.Reader) ProxiedVideoRefOwned {
	return ProxiedVideoRefOwned{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterProxiedVideoRefOwned) Lower(value ProxiedVideoRefOwned) C.RustBuffer {
	return LowerIntoRustBuffer[ProxiedVideoRefOwned](c, value)
}

func (c FfiConverterProxiedVideoRefOwned) LowerExternal(value ProxiedVideoRefOwned) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ProxiedVideoRefOwned](c, value))
}

func (c FfiConverterProxiedVideoRefOwned) Write(writer io.Writer, value ProxiedVideoRefOwned) {
	FfiConverterStringINSTANCE.Write(writer, value.Path)
	FfiConverterStringINSTANCE.Write(writer, value.Alt)
}

type FfiDestroyerProxiedVideoRefOwned struct{}

func (_ FfiDestroyerProxiedVideoRefOwned) Destroy(value ProxiedVideoRefOwned) {
	value.Destroy()
}

// One arm of `personalization-trained-factor-publish-kind-select`: the
// artifact kind the publish writes, and the text the picker paints.
//
// The [`backup_destination_kind_options`] shape verbatim — a raw-value picker
// whose `value` is the wire discriminator and whose `label` is the same
// [`LocalizedText`] every other surface uses for that kind. Sharing it is
// priority #2 plus the specific select hazard that precedent records: **the
// option a user picks and the words they read back must be the same text**,
// and seven apps each hand-writing a two-item list is seven chances to drift.
type PublishKindOption struct {
	// The `artifact_kind` this option publishes as — never a per-app literal.
	Value string
	// The option text, deliberately the same [`LocalizedText`]
	// [`publish_kind_label`] returns.
	Label LocalizedText
}

func (r *PublishKindOption) Destroy() {
	FfiDestroyerString{}.Destroy(r.Value)
	FfiDestroyerLocalizedText{}.Destroy(r.Label)
}

type FfiConverterPublishKindOption struct{}

var FfiConverterPublishKindOptionINSTANCE = FfiConverterPublishKindOption{}

func (c FfiConverterPublishKindOption) Lift(rb RustBufferI) PublishKindOption {
	return LiftFromRustBuffer[PublishKindOption](c, rb)
}

func (c FfiConverterPublishKindOption) Read(reader io.Reader) PublishKindOption {
	return PublishKindOption{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterPublishKindOption) Lower(value PublishKindOption) C.RustBuffer {
	return LowerIntoRustBuffer[PublishKindOption](c, value)
}

func (c FfiConverterPublishKindOption) LowerExternal(value PublishKindOption) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[PublishKindOption](c, value))
}

func (c FfiConverterPublishKindOption) Write(writer io.Writer, value PublishKindOption) {
	FfiConverterStringINSTANCE.Write(writer, value.Value)
	FfiConverterLocalizedTextINSTANCE.Write(writer, value.Label)
}

type FfiDestroyerPublishKindOption struct{}

func (_ FfiDestroyerPublishKindOption) Destroy(value PublishKindOption) {
	value.Destroy()
}

// Owned mirror of [`QuotedPostEmbed`] — the borrowed type carries a lifetime tied to
// the source [`RenderDocument`], which can't cross UniFFI/wasm, so the FFI/wasm face
// of [`RenderDocument::quoted_post`] (`fauna-ffi`/`fauna-wasm`) returns this instead.
type QuotedPostEmbedOwned struct {
	PostId           string
	Author           string
	Body             string
	Verification     VerificationStatus
	AuthoringOrigin  AuthoringOriginStatus
	LegalTakedownRef *string
	// See [`RenderBlock::QuotedPost`]'s `not_found`. Defaulted on every face, so
	// a binding or a payload from before the field keeps reading `false`.
	NotFound bool
}

func (r *QuotedPostEmbedOwned) Destroy() {
	FfiDestroyerString{}.Destroy(r.PostId)
	FfiDestroyerString{}.Destroy(r.Author)
	FfiDestroyerString{}.Destroy(r.Body)
	FfiDestroyerVerificationStatus{}.Destroy(r.Verification)
	FfiDestroyerAuthoringOriginStatus{}.Destroy(r.AuthoringOrigin)
	FfiDestroyerOptionalString{}.Destroy(r.LegalTakedownRef)
	FfiDestroyerBool{}.Destroy(r.NotFound)
}

type FfiConverterQuotedPostEmbedOwned struct{}

var FfiConverterQuotedPostEmbedOwnedINSTANCE = FfiConverterQuotedPostEmbedOwned{}

func (c FfiConverterQuotedPostEmbedOwned) Lift(rb RustBufferI) QuotedPostEmbedOwned {
	return LiftFromRustBuffer[QuotedPostEmbedOwned](c, rb)
}

func (c FfiConverterQuotedPostEmbedOwned) Read(reader io.Reader) QuotedPostEmbedOwned {
	return QuotedPostEmbedOwned{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterVerificationStatusINSTANCE.Read(reader),
		FfiConverterAuthoringOriginStatusINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterQuotedPostEmbedOwned) Lower(value QuotedPostEmbedOwned) C.RustBuffer {
	return LowerIntoRustBuffer[QuotedPostEmbedOwned](c, value)
}

func (c FfiConverterQuotedPostEmbedOwned) LowerExternal(value QuotedPostEmbedOwned) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[QuotedPostEmbedOwned](c, value))
}

func (c FfiConverterQuotedPostEmbedOwned) Write(writer io.Writer, value QuotedPostEmbedOwned) {
	FfiConverterStringINSTANCE.Write(writer, value.PostId)
	FfiConverterStringINSTANCE.Write(writer, value.Author)
	FfiConverterStringINSTANCE.Write(writer, value.Body)
	FfiConverterVerificationStatusINSTANCE.Write(writer, value.Verification)
	FfiConverterAuthoringOriginStatusINSTANCE.Write(writer, value.AuthoringOrigin)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.LegalTakedownRef)
	FfiConverterBoolINSTANCE.Write(writer, value.NotFound)
}

type FfiDestroyerQuotedPostEmbedOwned struct{}

func (_ FfiDestroyerQuotedPostEmbedOwned) Destroy(value QuotedPostEmbedOwned) {
	value.Destroy()
}

// One option in a guardian policy editor's select (`family-safety.md` § Guardian
// policy pillar 1) — the canonical wire value plus the i18n key the client
// resolves. Mirrors `fauna_folders_machine::ConflictPolicyOption`'s shape:
// shared Rust owns the option *set*, the client owns the widget.
//
// `value` is a `String`, not a typed enum, on purpose — the same convention
// `FfiReachPolicy.feed_sources` follows: closed-enum FFI *fields* cross as
// strings. This is an option **catalog** (a UI picker descriptor), not a wire
// field, which is why it may be a `uniffi::Record` while the knob it describes
// stays a string.
type ReachPolicyOption struct {
	// The canonical wire/DB value ([`UnknownSenderMail::as_str`] /
	// [`FeedSources::as_str`]) — what the select writes to
	// `guardian_policies.*` and what the cross-app `select(id, value)` e2e
	// contract drives.
	Value string
	Label LocalizedText
}

func (r *ReachPolicyOption) Destroy() {
	FfiDestroyerString{}.Destroy(r.Value)
	FfiDestroyerLocalizedText{}.Destroy(r.Label)
}

type FfiConverterReachPolicyOption struct{}

var FfiConverterReachPolicyOptionINSTANCE = FfiConverterReachPolicyOption{}

func (c FfiConverterReachPolicyOption) Lift(rb RustBufferI) ReachPolicyOption {
	return LiftFromRustBuffer[ReachPolicyOption](c, rb)
}

func (c FfiConverterReachPolicyOption) Read(reader io.Reader) ReachPolicyOption {
	return ReachPolicyOption{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterReachPolicyOption) Lower(value ReachPolicyOption) C.RustBuffer {
	return LowerIntoRustBuffer[ReachPolicyOption](c, value)
}

func (c FfiConverterReachPolicyOption) LowerExternal(value ReachPolicyOption) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ReachPolicyOption](c, value))
}

func (c FfiConverterReachPolicyOption) Write(writer io.Writer, value ReachPolicyOption) {
	FfiConverterStringINSTANCE.Write(writer, value.Value)
	FfiConverterLocalizedTextINSTANCE.Write(writer, value.Label)
}

type FfiDestroyerReachPolicyOption struct{}

func (_ FfiDestroyerReachPolicyOption) Destroy(value ReachPolicyOption) {
	value.Destroy()
}

// Boundary-friendly flattening of [`relative_time`] for the FFI / wasm clients,
// so they never re-implement the bucket→key selection: `localized` is `Some`
// (render the key) for the four relative buckets; when it is `None`, render
// `absolute_epoch_ms` as a real date with the platform's native, locale-aware
// date formatter. (Linux can use [`relative_time`] + [`RelativeTimestamp::to_localized`]
// directly instead.)
type RelativeTimeDisplay struct {
	Localized       *LocalizedText
	AbsoluteEpochMs *int64
}

func (r *RelativeTimeDisplay) Destroy() {
	FfiDestroyerOptionalLocalizedText{}.Destroy(r.Localized)
	FfiDestroyerOptionalInt64{}.Destroy(r.AbsoluteEpochMs)
}

type FfiConverterRelativeTimeDisplay struct{}

var FfiConverterRelativeTimeDisplayINSTANCE = FfiConverterRelativeTimeDisplay{}

func (c FfiConverterRelativeTimeDisplay) Lift(rb RustBufferI) RelativeTimeDisplay {
	return LiftFromRustBuffer[RelativeTimeDisplay](c, rb)
}

func (c FfiConverterRelativeTimeDisplay) Read(reader io.Reader) RelativeTimeDisplay {
	return RelativeTimeDisplay{
		FfiConverterOptionalLocalizedTextINSTANCE.Read(reader),
		FfiConverterOptionalInt64INSTANCE.Read(reader),
	}
}

func (c FfiConverterRelativeTimeDisplay) Lower(value RelativeTimeDisplay) C.RustBuffer {
	return LowerIntoRustBuffer[RelativeTimeDisplay](c, value)
}

func (c FfiConverterRelativeTimeDisplay) LowerExternal(value RelativeTimeDisplay) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RelativeTimeDisplay](c, value))
}

func (c FfiConverterRelativeTimeDisplay) Write(writer io.Writer, value RelativeTimeDisplay) {
	FfiConverterOptionalLocalizedTextINSTANCE.Write(writer, value.Localized)
	FfiConverterOptionalInt64INSTANCE.Write(writer, value.AbsoluteEpochMs)
}

type FfiDestroyerRelativeTimeDisplay struct{}

func (_ FfiDestroyerRelativeTimeDisplay) Destroy(value RelativeTimeDisplay) {
	value.Destroy()
}

// Owned mirror of [`RemoteImageRef`] — same cross-boundary reason as
// [`QuotedPostEmbedOwned`], for the FFI/wasm face of [`RenderDocument::remote_images`].
type RemoteImageRefOwned struct {
	Url      string
	Alt      string
	Revealed bool
}

func (r *RemoteImageRefOwned) Destroy() {
	FfiDestroyerString{}.Destroy(r.Url)
	FfiDestroyerString{}.Destroy(r.Alt)
	FfiDestroyerBool{}.Destroy(r.Revealed)
}

type FfiConverterRemoteImageRefOwned struct{}

var FfiConverterRemoteImageRefOwnedINSTANCE = FfiConverterRemoteImageRefOwned{}

func (c FfiConverterRemoteImageRefOwned) Lift(rb RustBufferI) RemoteImageRefOwned {
	return LiftFromRustBuffer[RemoteImageRefOwned](c, rb)
}

func (c FfiConverterRemoteImageRefOwned) Read(reader io.Reader) RemoteImageRefOwned {
	return RemoteImageRefOwned{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterRemoteImageRefOwned) Lower(value RemoteImageRefOwned) C.RustBuffer {
	return LowerIntoRustBuffer[RemoteImageRefOwned](c, value)
}

func (c FfiConverterRemoteImageRefOwned) LowerExternal(value RemoteImageRefOwned) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RemoteImageRefOwned](c, value))
}

func (c FfiConverterRemoteImageRefOwned) Write(writer io.Writer, value RemoteImageRefOwned) {
	FfiConverterStringINSTANCE.Write(writer, value.Url)
	FfiConverterStringINSTANCE.Write(writer, value.Alt)
	FfiConverterBoolINSTANCE.Write(writer, value.Revealed)
}

type FfiDestroyerRemoteImageRefOwned struct{}

func (_ FfiDestroyerRemoteImageRefOwned) Destroy(value RemoteImageRefOwned) {
	value.Destroy()
}

// An ordered list of semantic [`RenderBlock`]s — one rendered body. The named type
// (rather than a bare `Vec<RenderBlock>` alias) gives snapshot fields a stable FFI
// record (`MessageSnapshot.document: RenderDocument`) and lets a list item be "a
// sub-document" without a second alias.
type RenderDocument struct {
	Blocks []RenderBlock
}

func (r *RenderDocument) Destroy() {
	FfiDestroyerSequenceRenderBlock{}.Destroy(r.Blocks)
}

type FfiConverterRenderDocument struct{}

var FfiConverterRenderDocumentINSTANCE = FfiConverterRenderDocument{}

func (c FfiConverterRenderDocument) Lift(rb RustBufferI) RenderDocument {
	return LiftFromRustBuffer[RenderDocument](c, rb)
}

func (c FfiConverterRenderDocument) Read(reader io.Reader) RenderDocument {
	return RenderDocument{
		FfiConverterSequenceRenderBlockINSTANCE.Read(reader),
	}
}

func (c FfiConverterRenderDocument) Lower(value RenderDocument) C.RustBuffer {
	return LowerIntoRustBuffer[RenderDocument](c, value)
}

func (c FfiConverterRenderDocument) LowerExternal(value RenderDocument) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RenderDocument](c, value))
}

func (c FfiConverterRenderDocument) Write(writer io.Writer, value RenderDocument) {
	FfiConverterSequenceRenderBlockINSTANCE.Write(writer, value.Blocks)
}

type FfiDestroyerRenderDocument struct{}

func (_ FfiDestroyerRenderDocument) Destroy(value RenderDocument) {
	value.Destroy()
}

// Owned mirror of [`ResolvedLinkPreview`] — same cross-boundary reason as
// [`QuotedPostEmbedOwned`], for the FFI/wasm face of
// [`RenderDocument::resolved_link_previews`].
type ResolvedLinkPreviewOwned struct {
	Url         string
	Title       string
	Description string
	ImageHash   *string
	Revealed    bool
}

func (r *ResolvedLinkPreviewOwned) Destroy() {
	FfiDestroyerString{}.Destroy(r.Url)
	FfiDestroyerString{}.Destroy(r.Title)
	FfiDestroyerString{}.Destroy(r.Description)
	FfiDestroyerOptionalString{}.Destroy(r.ImageHash)
	FfiDestroyerBool{}.Destroy(r.Revealed)
}

type FfiConverterResolvedLinkPreviewOwned struct{}

var FfiConverterResolvedLinkPreviewOwnedINSTANCE = FfiConverterResolvedLinkPreviewOwned{}

func (c FfiConverterResolvedLinkPreviewOwned) Lift(rb RustBufferI) ResolvedLinkPreviewOwned {
	return LiftFromRustBuffer[ResolvedLinkPreviewOwned](c, rb)
}

func (c FfiConverterResolvedLinkPreviewOwned) Read(reader io.Reader) ResolvedLinkPreviewOwned {
	return ResolvedLinkPreviewOwned{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterResolvedLinkPreviewOwned) Lower(value ResolvedLinkPreviewOwned) C.RustBuffer {
	return LowerIntoRustBuffer[ResolvedLinkPreviewOwned](c, value)
}

func (c FfiConverterResolvedLinkPreviewOwned) LowerExternal(value ResolvedLinkPreviewOwned) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ResolvedLinkPreviewOwned](c, value))
}

func (c FfiConverterResolvedLinkPreviewOwned) Write(writer io.Writer, value ResolvedLinkPreviewOwned) {
	FfiConverterStringINSTANCE.Write(writer, value.Url)
	FfiConverterStringINSTANCE.Write(writer, value.Title)
	FfiConverterStringINSTANCE.Write(writer, value.Description)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.ImageHash)
	FfiConverterBoolINSTANCE.Write(writer, value.Revealed)
}

type FfiDestroyerResolvedLinkPreviewOwned struct{}

func (_ FfiDestroyerResolvedLinkPreviewOwned) Destroy(value ResolvedLinkPreviewOwned) {
	value.Destroy()
}

// One rspamd symbol (rule) that fired, with its contribution as a milli-int
// (signed: ham rules are negative).
type RspamdRuleContribution struct {
	Rule string
	// The rule's score contribution × 1000.
	ScoreMilli int32
}

func (r *RspamdRuleContribution) Destroy() {
	FfiDestroyerString{}.Destroy(r.Rule)
	FfiDestroyerInt32{}.Destroy(r.ScoreMilli)
}

type FfiConverterRspamdRuleContribution struct{}

var FfiConverterRspamdRuleContributionINSTANCE = FfiConverterRspamdRuleContribution{}

func (c FfiConverterRspamdRuleContribution) Lift(rb RustBufferI) RspamdRuleContribution {
	return LiftFromRustBuffer[RspamdRuleContribution](c, rb)
}

func (c FfiConverterRspamdRuleContribution) Read(reader io.Reader) RspamdRuleContribution {
	return RspamdRuleContribution{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterInt32INSTANCE.Read(reader),
	}
}

func (c FfiConverterRspamdRuleContribution) Lower(value RspamdRuleContribution) C.RustBuffer {
	return LowerIntoRustBuffer[RspamdRuleContribution](c, value)
}

func (c FfiConverterRspamdRuleContribution) LowerExternal(value RspamdRuleContribution) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RspamdRuleContribution](c, value))
}

func (c FfiConverterRspamdRuleContribution) Write(writer io.Writer, value RspamdRuleContribution) {
	FfiConverterStringINSTANCE.Write(writer, value.Rule)
	FfiConverterInt32INSTANCE.Write(writer, value.ScoreMilli)
}

type FfiDestroyerRspamdRuleContribution struct{}

func (_ FfiDestroyerRspamdRuleContribution) Destroy(value RspamdRuleContribution) {
	value.Destroy()
}

// rspamd content-score result, all scores as milli-ints (no wire floats).
type RspamdScore struct {
	// rspamd's native score × 1000 (native range ~0–30).
	RawMilli int32
	// After applying `scaling_per_mille` (the value fed to mail-spam's 0–15
	// scale) × 1000.
	ScaledMilli int32
	// Names of every symbol that fired, sorted (JSON map order is unstable).
	FlaggedRules []string
	// Per-rule contributions, sorted by rule name.
	Breakdown []RspamdRuleContribution
}

func (r *RspamdScore) Destroy() {
	FfiDestroyerInt32{}.Destroy(r.RawMilli)
	FfiDestroyerInt32{}.Destroy(r.ScaledMilli)
	FfiDestroyerSequenceString{}.Destroy(r.FlaggedRules)
	FfiDestroyerSequenceRspamdRuleContribution{}.Destroy(r.Breakdown)
}

type FfiConverterRspamdScore struct{}

var FfiConverterRspamdScoreINSTANCE = FfiConverterRspamdScore{}

func (c FfiConverterRspamdScore) Lift(rb RustBufferI) RspamdScore {
	return LiftFromRustBuffer[RspamdScore](c, rb)
}

func (c FfiConverterRspamdScore) Read(reader io.Reader) RspamdScore {
	return RspamdScore{
		FfiConverterInt32INSTANCE.Read(reader),
		FfiConverterInt32INSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterSequenceRspamdRuleContributionINSTANCE.Read(reader),
	}
}

func (c FfiConverterRspamdScore) Lower(value RspamdScore) C.RustBuffer {
	return LowerIntoRustBuffer[RspamdScore](c, value)
}

func (c FfiConverterRspamdScore) LowerExternal(value RspamdScore) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RspamdScore](c, value))
}

func (c FfiConverterRspamdScore) Write(writer io.Writer, value RspamdScore) {
	FfiConverterInt32INSTANCE.Write(writer, value.RawMilli)
	FfiConverterInt32INSTANCE.Write(writer, value.ScaledMilli)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.FlaggedRules)
	FfiConverterSequenceRspamdRuleContributionINSTANCE.Write(writer, value.Breakdown)
}

type FfiDestroyerRspamdScore struct{}

func (_ FfiDestroyerRspamdScore) Destroy(value RspamdScore) {
	value.Destroy()
}

// One factor's row on the scoring-metadata bus.
//
// Metadata only — a score/label/verdict, never the content that produced it
// (`content-scoring.md` § bus rule). No `deny_unknown_fields`: entries are
// embedded in at-rest floors, and an older binary must keep decoding a floor
// whose entries grew an additive field (version-compatibility.md I2).
//
// A `uniffi::Record` so the Go MTA receives the rows
// [`perimeter_mail_score_rows`] mints at the ingest edge (the contract phase
// of the bus: the perimeter emits its own rows, the nest derives nothing).
type ScoreEntry struct {
	// Factor name — one of [`factor`]'s constants for the built-in mail
	// factors; community/admin labelers add theirs.
	Factor string
	// Integer per-mille (milli-int, signed — ham/negative contributions
	// allowed). The dag-cbor wire forbids floats.
	Score int64
	// Model-authority tier: [`TIER_USER`] / [`TIER_ADMIN`] /
	// [`TIER_COMMUNITY`] (`content-moderation-and-ranking.md` § The three
	// model-authority tiers).
	Tier uint8
	// The scorer's version watermark. Drives the re-score obligation: a
	// capability-holder drain compares this against the current
	// model-version registry and re-scores on a gap (frame § Tier-3;
	// consumer: the content-at-rest drain worker). Type matches
	// `IndexManifest.tokenizer_version` (`u32`), the sibling watermark.
	ScorerVersion uint32
}

func (r *ScoreEntry) Destroy() {
	FfiDestroyerString{}.Destroy(r.Factor)
	FfiDestroyerInt64{}.Destroy(r.Score)
	FfiDestroyerUint8{}.Destroy(r.Tier)
	FfiDestroyerUint32{}.Destroy(r.ScorerVersion)
}

type FfiConverterScoreEntry struct{}

var FfiConverterScoreEntryINSTANCE = FfiConverterScoreEntry{}

func (c FfiConverterScoreEntry) Lift(rb RustBufferI) ScoreEntry {
	return LiftFromRustBuffer[ScoreEntry](c, rb)
}

func (c FfiConverterScoreEntry) Read(reader io.Reader) ScoreEntry {
	return ScoreEntry{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterUint8INSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
	}
}

func (c FfiConverterScoreEntry) Lower(value ScoreEntry) C.RustBuffer {
	return LowerIntoRustBuffer[ScoreEntry](c, value)
}

func (c FfiConverterScoreEntry) LowerExternal(value ScoreEntry) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ScoreEntry](c, value))
}

func (c FfiConverterScoreEntry) Write(writer io.Writer, value ScoreEntry) {
	FfiConverterStringINSTANCE.Write(writer, value.Factor)
	FfiConverterInt64INSTANCE.Write(writer, value.Score)
	FfiConverterUint8INSTANCE.Write(writer, value.Tier)
	FfiConverterUint32INSTANCE.Write(writer, value.ScorerVersion)
}

type FfiDestroyerScoreEntry struct{}

func (_ FfiDestroyerScoreEntry) Destroy(value ScoreEntry) {
	value.Destroy()
}

// One item of a [`RenderBlock::TaskList`] — a GFM task-list entry (render-model.md § D7a).
// `checked` is the `[x]`/`[ ]` state; `blocks` is the item's sub-document — one `Paragraph`
// from markdown today, but block-level (like [`RenderBlock::BlockQuote`]'s `blocks`) so a
// nested list/quote inside an item composes. Sibling to a `ListBlock` item (which is a
// [`RenderDocument`]); both carry block content, so the client walkers paint them the same way
// (`item.blocks`) save the per-item checkbox marker.
type TaskItem struct {
	Checked bool
	Blocks  []RenderBlock
}

func (r *TaskItem) Destroy() {
	FfiDestroyerBool{}.Destroy(r.Checked)
	FfiDestroyerSequenceRenderBlock{}.Destroy(r.Blocks)
}

type FfiConverterTaskItem struct{}

var FfiConverterTaskItemINSTANCE = FfiConverterTaskItem{}

func (c FfiConverterTaskItem) Lift(rb RustBufferI) TaskItem {
	return LiftFromRustBuffer[TaskItem](c, rb)
}

func (c FfiConverterTaskItem) Read(reader io.Reader) TaskItem {
	return TaskItem{
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterSequenceRenderBlockINSTANCE.Read(reader),
	}
}

func (c FfiConverterTaskItem) Lower(value TaskItem) C.RustBuffer {
	return LowerIntoRustBuffer[TaskItem](c, value)
}

func (c FfiConverterTaskItem) LowerExternal(value TaskItem) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[TaskItem](c, value))
}

func (c FfiConverterTaskItem) Write(writer io.Writer, value TaskItem) {
	FfiConverterBoolINSTANCE.Write(writer, value.Checked)
	FfiConverterSequenceRenderBlockINSTANCE.Write(writer, value.Blocks)
}

type FfiDestroyerTaskItem struct{}

func (_ FfiDestroyerTaskItem) Destroy(value TaskItem) {
	value.Destroy()
}

// An in-app route (module doc).
type AppRoute interface {
	Destroy()
}

// Open `path` (forward-slash, folder-relative) of set `folder_id` with the
// share-link create surface open.
type AppRouteShareLink struct {
	FolderId int64
	Path     string
}

func (e AppRouteShareLink) Destroy() {
	FfiDestroyerInt64{}.Destroy(e.FolderId)
	FfiDestroyerString{}.Destroy(e.Path)
}

// Open the Folders page with set `folder_id`'s member-share flow open.
type AppRouteFolderShare struct {
	FolderId int64
}

func (e AppRouteFolderShare) Destroy() {
	FfiDestroyerInt64{}.Destroy(e.FolderId)
}

// Open the consent card for the pushed authorization request
// `request_uri`. Revealing the card is all it does — the approval stays
// the user's act on the card.
type AppRouteConsent struct {
	RequestUri string
}

func (e AppRouteConsent) Destroy() {
	FfiDestroyerString{}.Destroy(e.RequestUri)
}

type FfiConverterAppRoute struct{}

var FfiConverterAppRouteINSTANCE = FfiConverterAppRoute{}

func (c FfiConverterAppRoute) Lift(rb RustBufferI) AppRoute {
	return LiftFromRustBuffer[AppRoute](c, rb)
}

func (c FfiConverterAppRoute) Lower(value AppRoute) C.RustBuffer {
	return LowerIntoRustBuffer[AppRoute](c, value)
}

func (c FfiConverterAppRoute) LowerExternal(value AppRoute) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AppRoute](c, value))
}
func (FfiConverterAppRoute) Read(reader io.Reader) AppRoute {
	id := readInt32(reader)
	switch id {
	case 1:
		return AppRouteShareLink{
			FfiConverterInt64INSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 2:
		return AppRouteFolderShare{
			FfiConverterInt64INSTANCE.Read(reader),
		}
	case 3:
		return AppRouteConsent{
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterAppRoute.Read()", id))
	}
}

func (FfiConverterAppRoute) Write(writer io.Writer, value AppRoute) {
	switch variant_value := value.(type) {
	case AppRouteShareLink:
		writeInt32(writer, 1)
		FfiConverterInt64INSTANCE.Write(writer, variant_value.FolderId)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Path)
	case AppRouteFolderShare:
		writeInt32(writer, 2)
		FfiConverterInt64INSTANCE.Write(writer, variant_value.FolderId)
	case AppRouteConsent:
		writeInt32(writer, 3)
		FfiConverterStringINSTANCE.Write(writer, variant_value.RequestUri)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterAppRoute.Write", value))
	}
}

type FfiDestroyerAppRoute struct{}

func (_ FfiDestroyerAppRoute) Destroy(value AppRoute) {
	value.Destroy()
}

type ArcVerdict uint

const (
	ArcVerdictNone      ArcVerdict = 1
	ArcVerdictPass      ArcVerdict = 2
	ArcVerdictFail      ArcVerdict = 3
	ArcVerdictPermError ArcVerdict = 4
	ArcVerdictTempError ArcVerdict = 5
)

type FfiConverterArcVerdict struct{}

var FfiConverterArcVerdictINSTANCE = FfiConverterArcVerdict{}

func (c FfiConverterArcVerdict) Lift(rb RustBufferI) ArcVerdict {
	return LiftFromRustBuffer[ArcVerdict](c, rb)
}

func (c FfiConverterArcVerdict) Lower(value ArcVerdict) C.RustBuffer {
	return LowerIntoRustBuffer[ArcVerdict](c, value)
}

func (c FfiConverterArcVerdict) LowerExternal(value ArcVerdict) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ArcVerdict](c, value))
}
func (FfiConverterArcVerdict) Read(reader io.Reader) ArcVerdict {
	id := readInt32(reader)
	return ArcVerdict(id)
}

func (FfiConverterArcVerdict) Write(writer io.Writer, value ArcVerdict) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerArcVerdict struct{}

func (_ FfiDestroyerArcVerdict) Destroy(value ArcVerdict) {
}

// Whether a verified payload was authored by the account's **own identity key**
// or by a **delegated authoring sub-key** — the render-layer face of
// [`crate::encoding::AuthoringOrigin`], and the D10 audit surface
// (`atproto-pds-full.md` § Problem 1 → D10 → *Audit*, ratified 2026-07-29).
//
// The audit surface D10 ratified is *the delegated content itself, verified
// client-side* — never a nest-read log. `signer_auth` rides **outside** the
// signed bytes, so stripping it from a delegated value makes verification FAIL
// rather than read as [`Direct`]; masquerading delegated content as direct
// needs the identity key. Rendering this enum is therefore client-authoritative
// audit in `docs/goal/ui/nests.md`'s "never a nest read" sense.
//
// The marker (a `delegated-origin-badge`) renders **iff** [`Delegated`].
// [`Unknown`] is both the nest-index projection default (no envelope to verify
// — the [`VerificationStatus::Unchecked`] twin) *and* the verification-failed
// case: neither says anything trustworthy about origin, and collapsing them is
// deliberate, since a failed envelope's cert claim is exactly what must not be
// believed.
//
// **The delegated sub-key's `device_key` is deliberately NOT carried here.**
// There is exactly one authoring sub-key per account, so it can never name
// *which* external app wrote a post — no surface can render it, and exporting
// it to the apps would be the dark-capability class the Audit bullet warns of.
//
// [`Direct`]: AuthoringOriginStatus::Direct
// [`Delegated`]: AuthoringOriginStatus::Delegated
// [`Unknown`]: AuthoringOriginStatus::Unknown
type AuthoringOriginStatus uint

const (
	// No trustworthy origin answer — a nest-index projection this client never
	// decoded, or an envelope whose verification **failed**. **No badge.**
	AuthoringOriginStatusUnknown AuthoringOriginStatus = 1
	// The envelope verified under the author's **own identity key**. **No badge.**
	AuthoringOriginStatusDirect AuthoringOriginStatus = 2
	// The envelope verified under a **delegated authoring sub-key** carrying a
	// valid identity-signed `DeviceAuthorization` — an external app authored
	// this as the account. **Renders the `delegated-origin-badge`.**
	AuthoringOriginStatusDelegated AuthoringOriginStatus = 3
)

type FfiConverterAuthoringOriginStatus struct{}

var FfiConverterAuthoringOriginStatusINSTANCE = FfiConverterAuthoringOriginStatus{}

func (c FfiConverterAuthoringOriginStatus) Lift(rb RustBufferI) AuthoringOriginStatus {
	return LiftFromRustBuffer[AuthoringOriginStatus](c, rb)
}

func (c FfiConverterAuthoringOriginStatus) Lower(value AuthoringOriginStatus) C.RustBuffer {
	return LowerIntoRustBuffer[AuthoringOriginStatus](c, value)
}

func (c FfiConverterAuthoringOriginStatus) LowerExternal(value AuthoringOriginStatus) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AuthoringOriginStatus](c, value))
}
func (FfiConverterAuthoringOriginStatus) Read(reader io.Reader) AuthoringOriginStatus {
	id := readInt32(reader)
	return AuthoringOriginStatus(id)
}

func (FfiConverterAuthoringOriginStatus) Write(writer io.Writer, value AuthoringOriginStatus) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerAuthoringOriginStatus struct{}

func (_ FfiDestroyerAuthoringOriginStatus) Destroy(value AuthoringOriginStatus) {
}

// Why a `backup-audit-alert` banner is showing.
//
// Three of its arms are the plain-data mirror of the three alerting
// `AuditVerdict`s — the owner-side loop's own findings. The fifth,
// [`Self::SourceRegressed`], is a recovery notice rather than a finding
// against the destination. The fourth,
// [`Self::SelfReported`], has **no** verdict behind it: it carries a
// client-device custodian's own failing self-audit, which reaches the page on
// the status row instead (`backup-destinations.md` § Custodian contract,
// question 4). It is a banner reason rather than a verdict because the
// owner-side loop can never produce it: a custodian has no address to sample.
//
// It lives here rather than in `fauna-client-backup` because that crate
// *depends on* this one, so the format layer cannot name `AuditVerdict`. The
// single map between them is `AuditVerdict::alert_reason()`, and that verdict
// enum defines `is_alerting()` through it — so a fourth alerting verdict cannot
// be added without also giving it a banner, which is the failure mode this
// arrangement is designed to make impossible.
type BackupAuditAlertReason interface {
	Destroy()
}

// The destination's backed-up high-water lags what the client itself holds.
type BackupAuditAlertReasonFreshness struct {
	LagSecs int64
}

func (e BackupAuditAlertReasonFreshness) Destroy() {
	FfiDestroyerInt64{}.Destroy(e.LagSecs)
}

// A sampled record was missing, or present but unopenable under the
// owner's derived `NestBackupKey`.
type BackupAuditAlertReasonInclusion struct {
	Missing uint32
	Sampled uint32
}

func (e BackupAuditAlertReasonInclusion) Destroy() {
	FfiDestroyerUint32{}.Destroy(e.Missing)
	FfiDestroyerUint32{}.Destroy(e.Sampled)
}

// No audit pass has succeeded for longer than `AUDIT_OVERDUE`.
type BackupAuditAlertReasonOverdue struct {
	SinceSecs int64
}

func (e BackupAuditAlertReasonOverdue) Destroy() {
	FfiDestroyerInt64{}.Destroy(e.SinceSecs)
}

// A client-device custodian **reported its own copy as failing** at its
// last check-in (`AUDIT_STATE_FAILED`). Loud because a reported failure is
// the only failure signal that exists for a kind the owner cannot sample —
// and because withholding it silences the row, which reads as the
// sleeping-device case: the wrong alarm, thirty days late.
type BackupAuditAlertReasonSelfReported struct {
}

func (e BackupAuditAlertReasonSelfReported) Destroy() {
}

// The owner's own nest **went backwards** (restored from an older copy of
// its data) and this destination still holds what it lost — the recovery
// notice of an accepted source regression (`backup-restore.md` §
// Background Tasks → *Implementation status (audit loop)*, the
// accepted-regression bullet). `left_secs` is how long until the first of
// what it holds is reclaimed; absent when no deadline exists — the copy
// holds it as ordinary live rows, kept until recovered. Like
// `SelfReported` it has no verdict behind it: the destination did nothing
// wrong, so the audit still passes; `DestinationAuditRecord::alert_reasons`
// raises it from the client-local record while that record stands, and it
// stops by itself.
type BackupAuditAlertReasonSourceRegressed struct {
	LeftSecs *int64
}

func (e BackupAuditAlertReasonSourceRegressed) Destroy() {
	FfiDestroyerOptionalInt64{}.Destroy(e.LeftSecs)
}

type FfiConverterBackupAuditAlertReason struct{}

var FfiConverterBackupAuditAlertReasonINSTANCE = FfiConverterBackupAuditAlertReason{}

func (c FfiConverterBackupAuditAlertReason) Lift(rb RustBufferI) BackupAuditAlertReason {
	return LiftFromRustBuffer[BackupAuditAlertReason](c, rb)
}

func (c FfiConverterBackupAuditAlertReason) Lower(value BackupAuditAlertReason) C.RustBuffer {
	return LowerIntoRustBuffer[BackupAuditAlertReason](c, value)
}

func (c FfiConverterBackupAuditAlertReason) LowerExternal(value BackupAuditAlertReason) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[BackupAuditAlertReason](c, value))
}
func (FfiConverterBackupAuditAlertReason) Read(reader io.Reader) BackupAuditAlertReason {
	id := readInt32(reader)
	switch id {
	case 1:
		return BackupAuditAlertReasonFreshness{
			FfiConverterInt64INSTANCE.Read(reader),
		}
	case 2:
		return BackupAuditAlertReasonInclusion{
			FfiConverterUint32INSTANCE.Read(reader),
			FfiConverterUint32INSTANCE.Read(reader),
		}
	case 3:
		return BackupAuditAlertReasonOverdue{
			FfiConverterInt64INSTANCE.Read(reader),
		}
	case 4:
		return BackupAuditAlertReasonSelfReported{}
	case 5:
		return BackupAuditAlertReasonSourceRegressed{
			FfiConverterOptionalInt64INSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterBackupAuditAlertReason.Read()", id))
	}
}

func (FfiConverterBackupAuditAlertReason) Write(writer io.Writer, value BackupAuditAlertReason) {
	switch variant_value := value.(type) {
	case BackupAuditAlertReasonFreshness:
		writeInt32(writer, 1)
		FfiConverterInt64INSTANCE.Write(writer, variant_value.LagSecs)
	case BackupAuditAlertReasonInclusion:
		writeInt32(writer, 2)
		FfiConverterUint32INSTANCE.Write(writer, variant_value.Missing)
		FfiConverterUint32INSTANCE.Write(writer, variant_value.Sampled)
	case BackupAuditAlertReasonOverdue:
		writeInt32(writer, 3)
		FfiConverterInt64INSTANCE.Write(writer, variant_value.SinceSecs)
	case BackupAuditAlertReasonSelfReported:
		writeInt32(writer, 4)
	case BackupAuditAlertReasonSourceRegressed:
		writeInt32(writer, 5)
		FfiConverterOptionalInt64INSTANCE.Write(writer, variant_value.LeftSecs)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterBackupAuditAlertReason.Write", value))
	}
}

type FfiDestroyerBackupAuditAlertReason struct{}

func (_ FfiDestroyerBackupAuditAlertReason) Destroy(value BackupAuditAlertReason) {
	value.Destroy()
}

// ClamAV malware-scan verdict for one message.
type ClamavVerdict interface {
	Destroy()
}

// `stream: OK` — no signature matched. An affirmative claim about the
// message, so it is NOT the default: a verdict nobody computed is
// [`ClamavVerdict::NotScanned`].
type ClamavVerdictClean struct {
}

func (e ClamavVerdictClean) Destroy() {
}

// `stream: <signature> FOUND` — malware matched. Rides into the nest only
// on junk/tag actions; a `reject` action never reaches
// `ingest_inbound_mail`.
type ClamavVerdictInfected struct {
	Signature string
}

func (e ClamavVerdictInfected) Destroy() {
	FfiDestroyerString{}.Destroy(e.Signature)
}

// clamd returned an error (or an unrecognized reply). Must **NOT** be
// treated as clean — downstream this becomes a `Tempfail` (never
// allow-without-scan, per `mail-content-scanning.md` § Don't do these). A
// delivered message never carries this; it is present for the forensic
// report path.
type ClamavVerdictError struct {
	Detail string
}

func (e ClamavVerdictError) Destroy() {
	FfiDestroyerString{}.Destroy(e.Detail)
}

// Message exceeded `clamav_max_filesize` and was delivered unscanned, with
// `X-Fauna-Scan-Clamav: bypassed_oversize`. The Go side produces this (it
// knows the size cap) without a clamd round-trip.
type ClamavVerdictBypassedOversize struct {
}

func (e ClamavVerdictBypassedOversize) Destroy() {
}

// ClamAV never ran for this message: it came through a door that does not
// invoke the scan gate (submission 465/587 — the colleague twin and the
// sender's own Sent copy — `mail-content-scanning.md` § Implementation
// status today, the per-door census) or the admin disabled ClamAV. Not a
// verdict: no `clamav` bus row, no `X-Fauna-Scan-Clamav` header, and the
// nest records `message_scan_results.clamav_verdict = 'not_scanned'` only
// when rspamd ran (otherwise the message never reached the scan pipeline
// and gets no row at all). Also the wire default — a request that says
// nothing about ClamAV did not scan.
type ClamavVerdictNotScanned struct {
}

func (e ClamavVerdictNotScanned) Destroy() {
}

type FfiConverterClamavVerdict struct{}

var FfiConverterClamavVerdictINSTANCE = FfiConverterClamavVerdict{}

func (c FfiConverterClamavVerdict) Lift(rb RustBufferI) ClamavVerdict {
	return LiftFromRustBuffer[ClamavVerdict](c, rb)
}

func (c FfiConverterClamavVerdict) Lower(value ClamavVerdict) C.RustBuffer {
	return LowerIntoRustBuffer[ClamavVerdict](c, value)
}

func (c FfiConverterClamavVerdict) LowerExternal(value ClamavVerdict) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ClamavVerdict](c, value))
}
func (FfiConverterClamavVerdict) Read(reader io.Reader) ClamavVerdict {
	id := readInt32(reader)
	switch id {
	case 1:
		return ClamavVerdictClean{}
	case 2:
		return ClamavVerdictInfected{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 3:
		return ClamavVerdictError{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 4:
		return ClamavVerdictBypassedOversize{}
	case 5:
		return ClamavVerdictNotScanned{}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterClamavVerdict.Read()", id))
	}
}

func (FfiConverterClamavVerdict) Write(writer io.Writer, value ClamavVerdict) {
	switch variant_value := value.(type) {
	case ClamavVerdictClean:
		writeInt32(writer, 1)
	case ClamavVerdictInfected:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Signature)
	case ClamavVerdictError:
		writeInt32(writer, 3)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Detail)
	case ClamavVerdictBypassedOversize:
		writeInt32(writer, 4)
	case ClamavVerdictNotScanned:
		writeInt32(writer, 5)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterClamavVerdict.Write", value))
	}
}

type FfiDestroyerClamavVerdict struct{}

func (_ FfiDestroyerClamavVerdict) Destroy(value ClamavVerdict) {
	value.Destroy()
}

// Contextual display bucket for a conversation/thread last-activity time,
// computed in the *caller's local timezone* (`utc_offset_seconds`, e.g. `+7200`
// for UTC+2). Calendar-based, not duration-based: a message at 23:50 yesterday
// is [`Yesterday`](ConversationTimestamp::Yesterday) even though it is < 24 h
// old. Thresholds are fixed by `docs/goal/behavior/value-formatting.md`
// § Conversation timestamp (the authority).
type ConversationTimestamp interface {
	Destroy()
}

// Same local calendar day (and any future time): local wall-clock.
type ConversationTimestampToday struct {
	Hour   uint8
	Minute uint8
}

func (e ConversationTimestampToday) Destroy() {
	FfiDestroyerUint8{}.Destroy(e.Hour)
	FfiDestroyerUint8{}.Destroy(e.Minute)
}

// The previous local calendar day.
type ConversationTimestampYesterday struct {
}

func (e ConversationTimestampYesterday) Destroy() {
}

// 2–6 local days ago: the local weekday of `then` (`index` 0 = Monday).
type ConversationTimestampWeekday struct {
	Index uint8
}

func (e ConversationTimestampWeekday) Destroy() {
	FfiDestroyerUint8{}.Destroy(e.Index)
}

// `≥ 7` local days ago — the client renders a real date with its native,
// locale-aware date formatter (no shared i18n key fits a localized date).
type ConversationTimestampOlder struct {
	EpochMs int64
}

func (e ConversationTimestampOlder) Destroy() {
	FfiDestroyerInt64{}.Destroy(e.EpochMs)
}

type FfiConverterConversationTimestamp struct{}

var FfiConverterConversationTimestampINSTANCE = FfiConverterConversationTimestamp{}

func (c FfiConverterConversationTimestamp) Lift(rb RustBufferI) ConversationTimestamp {
	return LiftFromRustBuffer[ConversationTimestamp](c, rb)
}

func (c FfiConverterConversationTimestamp) Lower(value ConversationTimestamp) C.RustBuffer {
	return LowerIntoRustBuffer[ConversationTimestamp](c, value)
}

func (c FfiConverterConversationTimestamp) LowerExternal(value ConversationTimestamp) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ConversationTimestamp](c, value))
}
func (FfiConverterConversationTimestamp) Read(reader io.Reader) ConversationTimestamp {
	id := readInt32(reader)
	switch id {
	case 1:
		return ConversationTimestampToday{
			FfiConverterUint8INSTANCE.Read(reader),
			FfiConverterUint8INSTANCE.Read(reader),
		}
	case 2:
		return ConversationTimestampYesterday{}
	case 3:
		return ConversationTimestampWeekday{
			FfiConverterUint8INSTANCE.Read(reader),
		}
	case 4:
		return ConversationTimestampOlder{
			FfiConverterInt64INSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterConversationTimestamp.Read()", id))
	}
}

func (FfiConverterConversationTimestamp) Write(writer io.Writer, value ConversationTimestamp) {
	switch variant_value := value.(type) {
	case ConversationTimestampToday:
		writeInt32(writer, 1)
		FfiConverterUint8INSTANCE.Write(writer, variant_value.Hour)
		FfiConverterUint8INSTANCE.Write(writer, variant_value.Minute)
	case ConversationTimestampYesterday:
		writeInt32(writer, 2)
	case ConversationTimestampWeekday:
		writeInt32(writer, 3)
		FfiConverterUint8INSTANCE.Write(writer, variant_value.Index)
	case ConversationTimestampOlder:
		writeInt32(writer, 4)
		FfiConverterInt64INSTANCE.Write(writer, variant_value.EpochMs)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterConversationTimestamp.Write", value))
	}
}

type FfiDestroyerConversationTimestamp struct{}

func (_ FfiDestroyerConversationTimestamp) Destroy(value ConversationTimestamp) {
	value.Destroy()
}

type DkimVerdict interface {
	Destroy()
}
type DkimVerdictNone struct {
}

func (e DkimVerdictNone) Destroy() {
}

type DkimVerdictPass struct {
}

func (e DkimVerdictPass) Destroy() {
}

type DkimVerdictFail struct {
	Reason string
}

func (e DkimVerdictFail) Destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

type DkimVerdictNeutral struct {
}

func (e DkimVerdictNeutral) Destroy() {
}

type DkimVerdictPermError struct {
}

func (e DkimVerdictPermError) Destroy() {
}

type DkimVerdictTempError struct {
}

func (e DkimVerdictTempError) Destroy() {
}

type FfiConverterDkimVerdict struct{}

var FfiConverterDkimVerdictINSTANCE = FfiConverterDkimVerdict{}

func (c FfiConverterDkimVerdict) Lift(rb RustBufferI) DkimVerdict {
	return LiftFromRustBuffer[DkimVerdict](c, rb)
}

func (c FfiConverterDkimVerdict) Lower(value DkimVerdict) C.RustBuffer {
	return LowerIntoRustBuffer[DkimVerdict](c, value)
}

func (c FfiConverterDkimVerdict) LowerExternal(value DkimVerdict) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[DkimVerdict](c, value))
}
func (FfiConverterDkimVerdict) Read(reader io.Reader) DkimVerdict {
	id := readInt32(reader)
	switch id {
	case 1:
		return DkimVerdictNone{}
	case 2:
		return DkimVerdictPass{}
	case 3:
		return DkimVerdictFail{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 4:
		return DkimVerdictNeutral{}
	case 5:
		return DkimVerdictPermError{}
	case 6:
		return DkimVerdictTempError{}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterDkimVerdict.Read()", id))
	}
}

func (FfiConverterDkimVerdict) Write(writer io.Writer, value DkimVerdict) {
	switch variant_value := value.(type) {
	case DkimVerdictNone:
		writeInt32(writer, 1)
	case DkimVerdictPass:
		writeInt32(writer, 2)
	case DkimVerdictFail:
		writeInt32(writer, 3)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Reason)
	case DkimVerdictNeutral:
		writeInt32(writer, 4)
	case DkimVerdictPermError:
		writeInt32(writer, 5)
	case DkimVerdictTempError:
		writeInt32(writer, 6)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterDkimVerdict.Write", value))
	}
}

type FfiDestroyerDkimVerdict struct{}

func (_ FfiDestroyerDkimVerdict) Destroy(value DkimVerdict) {
	value.Destroy()
}

type DmarcPolicy uint

const (
	DmarcPolicyNone       DmarcPolicy = 1
	DmarcPolicyQuarantine DmarcPolicy = 2
	DmarcPolicyReject     DmarcPolicy = 3
)

type FfiConverterDmarcPolicy struct{}

var FfiConverterDmarcPolicyINSTANCE = FfiConverterDmarcPolicy{}

func (c FfiConverterDmarcPolicy) Lift(rb RustBufferI) DmarcPolicy {
	return LiftFromRustBuffer[DmarcPolicy](c, rb)
}

func (c FfiConverterDmarcPolicy) Lower(value DmarcPolicy) C.RustBuffer {
	return LowerIntoRustBuffer[DmarcPolicy](c, value)
}

func (c FfiConverterDmarcPolicy) LowerExternal(value DmarcPolicy) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[DmarcPolicy](c, value))
}
func (FfiConverterDmarcPolicy) Read(reader io.Reader) DmarcPolicy {
	id := readInt32(reader)
	return DmarcPolicy(id)
}

func (FfiConverterDmarcPolicy) Write(writer io.Writer, value DmarcPolicy) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerDmarcPolicy struct{}

func (_ FfiDestroyerDmarcPolicy) Destroy(value DmarcPolicy) {
}

type DmarcVerdict interface {
	Destroy()
}
type DmarcVerdictNone struct {
}

func (e DmarcVerdictNone) Destroy() {
}

type DmarcVerdictPass struct {
}

func (e DmarcVerdictPass) Destroy() {
}

type DmarcVerdictFail struct {
	Policy DmarcPolicy
}

func (e DmarcVerdictFail) Destroy() {
	FfiDestroyerDmarcPolicy{}.Destroy(e.Policy)
}

type DmarcVerdictPermError struct {
}

func (e DmarcVerdictPermError) Destroy() {
}

type DmarcVerdictTempError struct {
}

func (e DmarcVerdictTempError) Destroy() {
}

type FfiConverterDmarcVerdict struct{}

var FfiConverterDmarcVerdictINSTANCE = FfiConverterDmarcVerdict{}

func (c FfiConverterDmarcVerdict) Lift(rb RustBufferI) DmarcVerdict {
	return LiftFromRustBuffer[DmarcVerdict](c, rb)
}

func (c FfiConverterDmarcVerdict) Lower(value DmarcVerdict) C.RustBuffer {
	return LowerIntoRustBuffer[DmarcVerdict](c, value)
}

func (c FfiConverterDmarcVerdict) LowerExternal(value DmarcVerdict) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[DmarcVerdict](c, value))
}
func (FfiConverterDmarcVerdict) Read(reader io.Reader) DmarcVerdict {
	id := readInt32(reader)
	switch id {
	case 1:
		return DmarcVerdictNone{}
	case 2:
		return DmarcVerdictPass{}
	case 3:
		return DmarcVerdictFail{
			FfiConverterDmarcPolicyINSTANCE.Read(reader),
		}
	case 4:
		return DmarcVerdictPermError{}
	case 5:
		return DmarcVerdictTempError{}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterDmarcVerdict.Read()", id))
	}
}

func (FfiConverterDmarcVerdict) Write(writer io.Writer, value DmarcVerdict) {
	switch variant_value := value.(type) {
	case DmarcVerdictNone:
		writeInt32(writer, 1)
	case DmarcVerdictPass:
		writeInt32(writer, 2)
	case DmarcVerdictFail:
		writeInt32(writer, 3)
		FfiConverterDmarcPolicyINSTANCE.Write(writer, variant_value.Policy)
	case DmarcVerdictPermError:
		writeInt32(writer, 4)
	case DmarcVerdictTempError:
		writeInt32(writer, 5)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterDmarcVerdict.Write", value))
	}
}

type FfiDestroyerDmarcVerdict struct{}

func (_ FfiDestroyerDmarcVerdict) Destroy(value DmarcVerdict) {
	value.Destroy()
}

// One inline run, as a tree (`Bold`/`Italic`/`Link` carry nested inlines). This is a
// strict generalisation of the flat [`MdSpan`] (which carries `bold`/`italic`/`code` as
// bools on a single run): the markdown producer maps a flat span to at most one level of
// nesting, but the tree shape lets a future producer (e.g. full HTML) nest emphasis.
type Inline interface {
	Destroy()
}

// Plain text.
type InlineText struct {
	Text string
}

func (e InlineText) Destroy() {
	FfiDestroyerString{}.Destroy(e.Text)
}

// Bold (`**`/`__`) wrapping nested inlines.
type InlineBold struct {
	Inlines []Inline
}

func (e InlineBold) Destroy() {
	FfiDestroyerSequenceInline{}.Destroy(e.Inlines)
}

// Italic (`*`/`_`) wrapping nested inlines.
type InlineItalic struct {
	Inlines []Inline
}

func (e InlineItalic) Destroy() {
	FfiDestroyerSequenceInline{}.Destroy(e.Inlines)
}

// Inline “ `code` “ — raw, never re-parsed.
type InlineCode struct {
	Text string
}

func (e InlineCode) Destroy() {
	FfiDestroyerString{}.Destroy(e.Text)
}

// A `[label](href)` link (`href` is `http(s)` only, mirroring `parse_link`).
type InlineLink struct {
	Href    string
	Inlines []Inline
}

func (e InlineLink) Destroy() {
	FfiDestroyerString{}.Destroy(e.Href)
	FfiDestroyerSequenceInline{}.Destroy(e.Inlines)
}

type FfiConverterInline struct{}

var FfiConverterInlineINSTANCE = FfiConverterInline{}

func (c FfiConverterInline) Lift(rb RustBufferI) Inline {
	return LiftFromRustBuffer[Inline](c, rb)
}

func (c FfiConverterInline) Lower(value Inline) C.RustBuffer {
	return LowerIntoRustBuffer[Inline](c, value)
}

func (c FfiConverterInline) LowerExternal(value Inline) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[Inline](c, value))
}
func (FfiConverterInline) Read(reader io.Reader) Inline {
	id := readInt32(reader)
	switch id {
	case 1:
		return InlineText{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 2:
		return InlineBold{
			FfiConverterSequenceInlineINSTANCE.Read(reader),
		}
	case 3:
		return InlineItalic{
			FfiConverterSequenceInlineINSTANCE.Read(reader),
		}
	case 4:
		return InlineCode{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 5:
		return InlineLink{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterSequenceInlineINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterInline.Read()", id))
	}
}

func (FfiConverterInline) Write(writer io.Writer, value Inline) {
	switch variant_value := value.(type) {
	case InlineText:
		writeInt32(writer, 1)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Text)
	case InlineBold:
		writeInt32(writer, 2)
		FfiConverterSequenceInlineINSTANCE.Write(writer, variant_value.Inlines)
	case InlineItalic:
		writeInt32(writer, 3)
		FfiConverterSequenceInlineINSTANCE.Write(writer, variant_value.Inlines)
	case InlineCode:
		writeInt32(writer, 4)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Text)
	case InlineLink:
		writeInt32(writer, 5)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Href)
		FfiConverterSequenceInlineINSTANCE.Write(writer, variant_value.Inlines)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterInline.Write", value))
	}
}

type FfiDestroyerInline struct{}

func (_ FfiDestroyerInline) Destroy(value Inline) {
	value.Destroy()
}

// How hard a muted keyword mutes, as the muted-words page offers it
// (`docs/goal/ui/settings.md` § Muted words — the level picker): two levels
// over the per-mille weight the record keeps
// (`content-moderation-and-ranking.md` § Composition, the 2026-07-10 ruling
// and its 2026-10-02 presentation). The record stays the full `[−1000, 0]`
// range, so a finer control later is an additive change, never a migration.
type MutedKeywordLevel uint

const (
	// The full penalty ([`MUTED_KEYWORDS_PENALTY`]) — the default a new term
	// mutes at: a match collapses the post or message behind its reveal and
	// sinks the post in every ranked feed.
	MutedKeywordLevelHide MutedKeywordLevel = 1
	// [`MUTED_KEYWORD_SHOW_LESS_WEIGHT`] — a match sinks the post in a ranked
	// feed and nothing else: no collapse anywhere, and a conversation, which
	// is not ranked, shows the message as usual.
	MutedKeywordLevelShowLess MutedKeywordLevel = 2
)

type FfiConverterMutedKeywordLevel struct{}

var FfiConverterMutedKeywordLevelINSTANCE = FfiConverterMutedKeywordLevel{}

func (c FfiConverterMutedKeywordLevel) Lift(rb RustBufferI) MutedKeywordLevel {
	return LiftFromRustBuffer[MutedKeywordLevel](c, rb)
}

func (c FfiConverterMutedKeywordLevel) Lower(value MutedKeywordLevel) C.RustBuffer {
	return LowerIntoRustBuffer[MutedKeywordLevel](c, value)
}

func (c FfiConverterMutedKeywordLevel) LowerExternal(value MutedKeywordLevel) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[MutedKeywordLevel](c, value))
}
func (FfiConverterMutedKeywordLevel) Read(reader io.Reader) MutedKeywordLevel {
	id := readInt32(reader)
	return MutedKeywordLevel(id)
}

func (FfiConverterMutedKeywordLevel) Write(writer io.Writer, value MutedKeywordLevel) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerMutedKeywordLevel struct{}

func (_ FfiDestroyerMutedKeywordLevel) Destroy(value MutedKeywordLevel) {
}

// The nest's NAT mode: is the box internet-reachable?
//
// - `Public` — internet-facing (typical VPS): serves federation/MX/MUA
// endpoints on a public domain, obtains ACME certificates, relay side of
// pairing.
// - `Private` — behind NAT (home box): no MTA, no ACME, LAN-only IMAP/CalDAV
// binds, pairs with a public nest as its relay.
type NodeMode uint

const (
	NodeModePublic  NodeMode = 1
	NodeModePrivate NodeMode = 2
)

type FfiConverterNodeMode struct{}

var FfiConverterNodeModeINSTANCE = FfiConverterNodeMode{}

func (c FfiConverterNodeMode) Lift(rb RustBufferI) NodeMode {
	return LiftFromRustBuffer[NodeMode](c, rb)
}

func (c FfiConverterNodeMode) Lower(value NodeMode) C.RustBuffer {
	return LowerIntoRustBuffer[NodeMode](c, value)
}

func (c FfiConverterNodeMode) LowerExternal(value NodeMode) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[NodeMode](c, value))
}
func (FfiConverterNodeMode) Read(reader io.Reader) NodeMode {
	id := readInt32(reader)
	return NodeMode(id)
}

func (FfiConverterNodeMode) Write(writer io.Writer, value NodeMode) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerNodeMode struct{}

func (_ FfiDestroyerNodeMode) Destroy(value NodeMode) {
}

// The semantic category of a notification type ([`NotifType`]).
// `Unknown` covers every type with no icon of its own, and every type this
// build does not name.
//
// Exported as a `uniffi::Enum` for the same reason [`crate::source_glyph::SourceGlyph`]
// is: `notifications.md` § Where logic lives makes shared Rust the owner of the
// *categorisation* and leaves the icon itself to app glue, because icons are
// platform-native assets. A native app switches on this enum directly and picks
// its own symbol (apple: SF Symbols; android: Material icons); linux and web take
// the [`Self::emoji`] their shared asset family wants.
//
// ⚠ Variants are APPENDED, never inserted: UniFFI numbers them in declaration
// order, so reordering renumbers every existing discriminant — the same
// constraint `SourceGlyph` records for its own trailing `Archive`.
type NotificationGlyph uint

const (
	NotificationGlyphMessage     NotificationGlyph = 1
	NotificationGlyphMention     NotificationGlyph = 2
	NotificationGlyphFollow      NotificationGlyph = 3
	NotificationGlyphEventInvite NotificationGlyph = 4
	NotificationGlyphGroupInvite NotificationGlyph = 5
	NotificationGlyphKnock       NotificationGlyph = 6
	NotificationGlyphReply       NotificationGlyph = 7
	NotificationGlyphLike        NotificationGlyph = 8
	NotificationGlyphUnknown     NotificationGlyph = 9
	// An abuse report: an admin's doorbell (`abuse_report.received`) or the
	// reporter's outcome (`abuse_report.resolved`) — `moderation.md` § App
	// surface. Appended after `Unknown` per the rule above.
	NotificationGlyphReport NotificationGlyph = 10
)

type FfiConverterNotificationGlyph struct{}

var FfiConverterNotificationGlyphINSTANCE = FfiConverterNotificationGlyph{}

func (c FfiConverterNotificationGlyph) Lift(rb RustBufferI) NotificationGlyph {
	return LiftFromRustBuffer[NotificationGlyph](c, rb)
}

func (c FfiConverterNotificationGlyph) Lower(value NotificationGlyph) C.RustBuffer {
	return LowerIntoRustBuffer[NotificationGlyph](c, value)
}

func (c FfiConverterNotificationGlyph) LowerExternal(value NotificationGlyph) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[NotificationGlyph](c, value))
}
func (FfiConverterNotificationGlyph) Read(reader io.Reader) NotificationGlyph {
	id := readInt32(reader)
	return NotificationGlyph(id)
}

func (FfiConverterNotificationGlyph) Write(writer io.Writer, value NotificationGlyph) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerNotificationGlyph struct{}

func (_ FfiDestroyerNotificationGlyph) Destroy(value NotificationGlyph) {
}

// The viewer's subscription status for one offered tier — the
// `subscription-offers-section` per-tier status badge on **another** actor's
// profile (`docs/goal/ui/profile.md` § Layout & flow → *Another's profile*;
// `docs/goal/behavior/monetization.md` § Pillar 1). A *derived* viewmodel enum
// (not a wire type): the offers browse reads `offers.list` (`Vec<TierItem>`) +
// `status.get` (`StatusGetReply { tier: Option<String>, .. }`), and per tier
// derives this status. `status.get` carries **no** pending discriminant — an
// OTHER profile sees only the viewer's *confirmed* tier — so `Pending` is purely
// a transient post-click state (subscribe → `Queued`), tracked client-side.
//
// Lifts the five per-app derivations onto one source of truth (priority
// #2/#4): web's inline map, linux's split 2-case `status_text` + a separate
// post-click overlay, and the android/apple/windows **local** `OfferStatus` /
// `OfferStatusKind` enums — all a strict superset, converged here on the richest
// (native-enum) shape. Clients branch on this enum for the label **and** (where
// they choose to) the Subscribe button state; the button enable/disable *policy*
// stays an idiomatic per-app UX choice (android disables on `Active`; web /
// linux / apple / windows leave it enabled). See profile.md / monetization.md
// § Where logic lives.
type OfferStatus uint

const (
	// The viewer neither holds nor has a pending request for this tier.
	OfferStatusNone OfferStatus = 1
	// A transient post-click `Queued` state (encrypted-mode subscribe), shown
	// until the next `status.get` read confirms or drops it.
	OfferStatusPending OfferStatus = 2
	// The viewer's confirmed held tier (`status.get` reports this tier).
	OfferStatusActive OfferStatus = 3
)

type FfiConverterOfferStatus struct{}

var FfiConverterOfferStatusINSTANCE = FfiConverterOfferStatus{}

func (c FfiConverterOfferStatus) Lift(rb RustBufferI) OfferStatus {
	return LiftFromRustBuffer[OfferStatus](c, rb)
}

func (c FfiConverterOfferStatus) Lower(value OfferStatus) C.RustBuffer {
	return LowerIntoRustBuffer[OfferStatus](c, value)
}

func (c FfiConverterOfferStatus) LowerExternal(value OfferStatus) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[OfferStatus](c, value))
}
func (FfiConverterOfferStatus) Read(reader io.Reader) OfferStatus {
	id := readInt32(reader)
	return OfferStatus(id)
}

func (FfiConverterOfferStatus) Write(writer io.Writer, value OfferStatus) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerOfferStatus struct{}

func (_ FfiDestroyerOfferStatus) Destroy(value OfferStatus) {
}

// Resolution state of a link-preview embed. Defined now as the leaf the model names;
// the `LinkPreview` block that carries it is a P4 feature (render-model.md § D4), where
// the manager resolves metadata off-thread and re-emits with `Resolved`.
type PreviewState interface {
	Destroy()
}

// Metadata fetch in flight.
type PreviewStateResolving struct {
}

func (e PreviewStateResolving) Destroy() {
}

// Resolved metadata. `image_hash` is the (optional) fetched preview image. Even though
// the og:image is a **content-addressed blob served by this client's OWN nest** (the
// nest fetched it server-side — `fauna_protocol::linkpreview`), so painting it never
// phones home to a third party, it carries the **same blocked-by-default reveal posture
// as any [`RenderBlock::RemoteImage`]** (render-model.md § D4 — user-ratified
// 2026-06-27: the og:image hides behind the post's existing remote-content reveal). The
// card's title / description / domain always show; only the image is gated.
type PreviewStateResolved struct {
	Title       string
	Description string
	ImageHash   *string
	Revealed    bool
}

func (e PreviewStateResolved) Destroy() {
	FfiDestroyerString{}.Destroy(e.Title)
	FfiDestroyerString{}.Destroy(e.Description)
	FfiDestroyerOptionalString{}.Destroy(e.ImageHash)
	FfiDestroyerBool{}.Destroy(e.Revealed)
}

// The fetch failed (a generic, non-retried failure for render purposes).
type PreviewStateFailed struct {
}

func (e PreviewStateFailed) Destroy() {
}

type FfiConverterPreviewState struct{}

var FfiConverterPreviewStateINSTANCE = FfiConverterPreviewState{}

func (c FfiConverterPreviewState) Lift(rb RustBufferI) PreviewState {
	return LiftFromRustBuffer[PreviewState](c, rb)
}

func (c FfiConverterPreviewState) Lower(value PreviewState) C.RustBuffer {
	return LowerIntoRustBuffer[PreviewState](c, value)
}

func (c FfiConverterPreviewState) LowerExternal(value PreviewState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[PreviewState](c, value))
}
func (FfiConverterPreviewState) Read(reader io.Reader) PreviewState {
	id := readInt32(reader)
	switch id {
	case 1:
		return PreviewStateResolving{}
	case 2:
		return PreviewStateResolved{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterOptionalStringINSTANCE.Read(reader),
			FfiConverterBoolINSTANCE.Read(reader),
		}
	case 3:
		return PreviewStateFailed{}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterPreviewState.Read()", id))
	}
}

func (FfiConverterPreviewState) Write(writer io.Writer, value PreviewState) {
	switch variant_value := value.(type) {
	case PreviewStateResolving:
		writeInt32(writer, 1)
	case PreviewStateResolved:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Title)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Description)
		FfiConverterOptionalStringINSTANCE.Write(writer, variant_value.ImageHash)
		FfiConverterBoolINSTANCE.Write(writer, variant_value.Revealed)
	case PreviewStateFailed:
		writeInt32(writer, 3)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterPreviewState.Write", value))
	}
}

type FfiDestroyerPreviewState struct{}

func (_ FfiDestroyerPreviewState) Destroy(value PreviewState) {
	value.Destroy()
}

// Uniform relative-time buckets. Thresholds are fixed by
// `docs/goal/behavior/value-formatting.md` § Relative time (the authority).
type RelativeTimestamp interface {
	Destroy()
}
type RelativeTimestampJustNow struct {
}

func (e RelativeTimestampJustNow) Destroy() {
}

type RelativeTimestampMinutesAgo struct {
	N uint32
}

func (e RelativeTimestampMinutesAgo) Destroy() {
	FfiDestroyerUint32{}.Destroy(e.N)
}

type RelativeTimestampHoursAgo struct {
	N uint32
}

func (e RelativeTimestampHoursAgo) Destroy() {
	FfiDestroyerUint32{}.Destroy(e.N)
}

type RelativeTimestampDaysAgo struct {
	N uint32
}

func (e RelativeTimestampDaysAgo) Destroy() {
	FfiDestroyerUint32{}.Destroy(e.N)
}

// `≥ 7 d` old — the client renders a real date with its native, locale-aware
// date formatter (no shared i18n key fits a localized absolute date).
type RelativeTimestampAbsolute struct {
	EpochMs int64
}

func (e RelativeTimestampAbsolute) Destroy() {
	FfiDestroyerInt64{}.Destroy(e.EpochMs)
}

type FfiConverterRelativeTimestamp struct{}

var FfiConverterRelativeTimestampINSTANCE = FfiConverterRelativeTimestamp{}

func (c FfiConverterRelativeTimestamp) Lift(rb RustBufferI) RelativeTimestamp {
	return LiftFromRustBuffer[RelativeTimestamp](c, rb)
}

func (c FfiConverterRelativeTimestamp) Lower(value RelativeTimestamp) C.RustBuffer {
	return LowerIntoRustBuffer[RelativeTimestamp](c, value)
}

func (c FfiConverterRelativeTimestamp) LowerExternal(value RelativeTimestamp) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RelativeTimestamp](c, value))
}
func (FfiConverterRelativeTimestamp) Read(reader io.Reader) RelativeTimestamp {
	id := readInt32(reader)
	switch id {
	case 1:
		return RelativeTimestampJustNow{}
	case 2:
		return RelativeTimestampMinutesAgo{
			FfiConverterUint32INSTANCE.Read(reader),
		}
	case 3:
		return RelativeTimestampHoursAgo{
			FfiConverterUint32INSTANCE.Read(reader),
		}
	case 4:
		return RelativeTimestampDaysAgo{
			FfiConverterUint32INSTANCE.Read(reader),
		}
	case 5:
		return RelativeTimestampAbsolute{
			FfiConverterInt64INSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterRelativeTimestamp.Read()", id))
	}
}

func (FfiConverterRelativeTimestamp) Write(writer io.Writer, value RelativeTimestamp) {
	switch variant_value := value.(type) {
	case RelativeTimestampJustNow:
		writeInt32(writer, 1)
	case RelativeTimestampMinutesAgo:
		writeInt32(writer, 2)
		FfiConverterUint32INSTANCE.Write(writer, variant_value.N)
	case RelativeTimestampHoursAgo:
		writeInt32(writer, 3)
		FfiConverterUint32INSTANCE.Write(writer, variant_value.N)
	case RelativeTimestampDaysAgo:
		writeInt32(writer, 4)
		FfiConverterUint32INSTANCE.Write(writer, variant_value.N)
	case RelativeTimestampAbsolute:
		writeInt32(writer, 5)
		FfiConverterInt64INSTANCE.Write(writer, variant_value.EpochMs)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterRelativeTimestamp.Write", value))
	}
}

type FfiDestroyerRelativeTimestamp struct{}

func (_ FfiDestroyerRelativeTimestamp) Destroy(value RelativeTimestamp) {
	value.Destroy()
}

// A block-level node. The text variants are the typed form of today's
// [`MdBlock`] kinds; `Image`/`RemoteImage` are the embeds markdown produces (every
// `![alt](url)` is a **remote** http(s) image, so the markdown producer only ever emits
// `RemoteImage` — `Image` carries already-fetched, trusted media from a snapshot
// producer in a later phase). Embeds appear **in body order**, never appended after the
// text by client glue (render-model.md § D2).
type RenderBlock interface {
	Destroy()
}

// A paragraph of inline content.
type RenderBlockParagraph struct {
	Inlines []Inline
}

func (e RenderBlockParagraph) Destroy() {
	FfiDestroyerSequenceInline{}.Destroy(e.Inlines)
}

// An ATX heading, `level` 1–4 (mirrors `MdBlock`'s heading levels).
type RenderBlockHeading struct {
	Level   uint8
	Inlines []Inline
}

func (e RenderBlockHeading) Destroy() {
	FfiDestroyerUint8{}.Destroy(e.Level)
	FfiDestroyerSequenceInline{}.Destroy(e.Inlines)
}

// An (un)ordered list. Each item is a sub-[`RenderDocument`] so an item can hold
// block-level content (the richest existing pattern — priority #4); a markdown item
// is one `Paragraph`. An ordered list renumbers from 1 at render (it carries no
// item numbers), matching `<ol>` and `markdown::parse_markdown`.
//
// Named `ListBlock`, not `List`: a `uniffi::Enum` variant named `List` generates a
// nested Kotlin class `RenderBlock.List` that **shadows `kotlin.collections.List`**,
// so every `List<…>` field in the generated sealed class fails to compile (caught when
// android first walked the document — render-model.md § The model).
type RenderBlockListBlock struct {
	Ordered bool
	Items   []RenderDocument
}

func (e RenderBlockListBlock) Destroy() {
	FfiDestroyerBool{}.Destroy(e.Ordered)
	FfiDestroyerSequenceRenderDocument{}.Destroy(e.Items)
}

// A GFM task list (`- [ ] …` / `- [x] …`) — a **sibling** of [`ListBlock`](Self::ListBlock)
// so a plain bullet list is untouched (render-model.md § D7a). Each [`TaskItem`] is a
// sub-document exactly like a `ListBlock` item, so nested and mixed bullet/checkbox lists
// compose: the markdown producer folds a run of task items into one `TaskList` and an
// adjacent run of plain bullets into a sibling `ListBlock`. Named `TaskList` (collision-free
// in all four bindings, unlike `List` → `ListBlock`). The read-side walk paints a **static**
// checked/unchecked box per item; the *interactive* checkbox is the Notes editor's job
// (tracked internally, § D7), not this display
// render. Not produced by any snapshot projection — only the markdown body producer emits it.
type RenderBlockTaskList struct {
	Items []TaskItem
}

func (e RenderBlockTaskList) Destroy() {
	FfiDestroyerSequenceTaskItem{}.Destroy(e.Items)
}

// A fenced code block. `lang` is the info-string language (always `None` from
// markdown today — `parse_markdown` does not capture it yet); `text` is the raw,
// un-rendered code.
type RenderBlockCodeBlock struct {
	Lang *string
	Text string
}

func (e RenderBlockCodeBlock) Destroy() {
	FfiDestroyerOptionalString{}.Destroy(e.Lang)
	FfiDestroyerString{}.Destroy(e.Text)
}

// A block quote. Markdown emits one quoted line per block; the nested `blocks` shape
// admits richer quotes from future producers.
type RenderBlockBlockQuote struct {
	Blocks []RenderBlock
}

func (e RenderBlockBlockQuote) Destroy() {
	FfiDestroyerSequenceRenderBlock{}.Destroy(e.Blocks)
}

// Trusted, already-fetched media addressed by content hash. **Not** produced by the
// markdown path (markdown images are remote); emitted by the attachment/media
// producers in P2/P3.
type RenderBlockImage struct {
	Hash string
	Alt  string
}

func (e RenderBlockImage) Destroy() {
	FfiDestroyerString{}.Destroy(e.Hash)
	FfiDestroyerString{}.Destroy(e.Alt)
}

// Trusted, already-fetched **video** addressed by content hash — the typed sibling of
// [`Image`](Self::Image) (render-model.md § Implementation status today → *the D6 media
// fold is UNTYPED*, closed by the D7a new-variant recipe). Before this variant existed
// the feed's media fold reduced every `MediaItem` to its blob hash alone, so no
// document-painting app could tell an mp4 from a png and `video-thumbnail` was
// unbuildable on all 7 apps; web painted it only by calling `decode_post` a *second*
// time in app code and branching on `media_type` itself.
//
// A **sibling variant, not a field on `Image`**, exactly as D7a prescribes: being new it
// breaks the exhaustive app walkers at compile time (the safety net), while leaving every
// existing `Image` construction and paint untouched.
//
// Carries the same two fields as [`Image`](Self::Image) and no more, because § The boundary
// admits structure/role/content/state and never a player: an app paints its
// `video-thumbnail` from `hash` through its own blob loader, the same async byte-load as
// `post-image`. Deliberately **no** poster/dimensions field — `MediaItem::thumbnail` and
// `::dimensions` are `None` from every writer we have (see `fauna_feed`'s
// `media_item_from_staged`), so such a field would be dead on arrival; it is additive
// later if a writer ever populates one. Deliberately no mime either: the image-vs-video
// branch is made *once* in the shared fold, which is the whole point of the variant.
type RenderBlockVideo struct {
	Hash string
	Alt  string
}

func (e RenderBlockVideo) Destroy() {
	FfiDestroyerString{}.Destroy(e.Hash)
	FfiDestroyerString{}.Destroy(e.Alt)
}

// A bridged post's own image attachment, served by the reader's OWN nest at a
// nest-relative, already-proxied `path` (`/api/v1/media/proxy?url=…` for ActivityPub
// and nostr, `/api/v1/bluesky/media?url=…` for Bluesky) — never an absolute URL
// (render-model.md § D6c; the path form is bridges.md § Unified feed ingestion
// ruling 4's). The nest-served sibling of [`Image`](Self::Image), a new variant per the
// D7a recipe so every exhaustive walker breaks at compile time.
//
// **Not [`RemoteImage`](Self::RemoteImage):** that url is a third-party origin the device
// dials itself, only after the D3 reveal; this path is fetched from the reader's own nest
// with the session bearer, exactly like `/api/v1/blob/<hash>`. **Not `Image`:** a path is
// not a content hash. It paints in the `post-image` slot immediately (the user-ruled D6c
// posture, 2026-09-30): no `revealed` flag, never counted by
// [`has_blocked_remote_images`](RenderDocument::has_blocked_remote_images).
type RenderBlockProxiedImage struct {
	Path string
	Alt  string
}

func (e RenderBlockProxiedImage) Destroy() {
	FfiDestroyerString{}.Destroy(e.Path)
	FfiDestroyerString{}.Destroy(e.Alt)
}

// A bridged post's own **video** attachment, served by the reader's own nest at the same
// nest-relative, already-proxied `path` form as [`ProxiedImage`](Self::ProxiedImage)
// (render-model.md § D6c → *Proxied video*). It completes the 2×2: [`Image`](Self::Image)
// / [`Video`](Self::Video) by content hash, `ProxiedImage` / `ProxiedVideo` by path. A
// sibling variant per the D7a recipe, never a flag on `ProxiedImage` (an app must never
// paint a video into an image element) and never a `Video` with an empty hash (a path is
// not a content address).
//
// Same posture as `ProxiedImage`: paints immediately in the `video-thumbnail` slot, no
// `revealed` flag, never counted by
// [`has_blocked_remote_images`](RenderDocument::has_blocked_remote_images). No app
// byte-loads it for a thumbnail — no writer supplies a poster frame, and the proxied twin
// of a first-frame grab would be a full bearer fetch per card.
type RenderBlockProxiedVideo struct {
	Path string
	Alt  string
}

func (e RenderBlockProxiedVideo) Destroy() {
	FfiDestroyerString{}.Destroy(e.Path)
	FfiDestroyerString{}.Destroy(e.Alt)
}

// A remote `![alt](url)` image (`http(s)` only). `revealed` is **false** by default —
// the privacy posture for untrusted inbound content (html-mail.md § Rendering /
// § Security). In P2 the reveal flag is projected from a manager-owned in-memory set
// (render-model.md § D3); the parser never fetches the url.
type RenderBlockRemoteImage struct {
	Url      string
	Alt      string
	Revealed bool
}

func (e RenderBlockRemoteImage) Destroy() {
	FfiDestroyerString{}.Destroy(e.Url)
	FfiDestroyerString{}.Destroy(e.Alt)
	FfiDestroyerBool{}.Destroy(e.Revealed)
}

// A first-class attachment node — the typed, in-body-order projection of
// `fauna_conversations::AttachmentSnapshot` (render-model.md § D2: embeds are
// blocks, not sibling snapshot fields). A **strict superset** of that snapshot's
// fields (priority #4) so no render data is lost. `blob_hash` is the content
// handle the client resolves to bytes through its existing
// `attachment_bytes(blob_hash)` loader — the *resolution* path is unchanged; only
// the *placement* moves into the document. Not produced by the markdown path; the
// conversations manager appends one per attachment after the text body (the
// `[body] → [attachments]` order all 7 apps already render). The variant name
// is `Attachment` (no stdlib collision, unlike `List` → `ListBlock`).
type RenderBlockAttachment struct {
	BlobHash  string
	Filename  string
	MimeType  string
	SizeBytes uint64
	IsImage   bool
	C2pa      bool
}

func (e RenderBlockAttachment) Destroy() {
	FfiDestroyerString{}.Destroy(e.BlobHash)
	FfiDestroyerString{}.Destroy(e.Filename)
	FfiDestroyerString{}.Destroy(e.MimeType)
	FfiDestroyerUint64{}.Destroy(e.SizeBytes)
	FfiDestroyerBool{}.Destroy(e.IsImage)
	FfiDestroyerBool{}.Destroy(e.C2pa)
}

// A first-class link-preview embed — a new **async-resolved** node
// (render-model.md § D4). The body producer emits one in
// [`PreviewState::Resolving`] for a **standalone bare-URL paragraph** (a
// paragraph that is a single [`Inline::Link`] whose visible text equals its
// `href`); the inline link itself **stays** in the paragraph and this preview
// renders as an *additional* block below it, so a client that doesn't paint the
// card still shows the link. The page manager then resolves the metadata
// off-thread via the authenticated `fauna.linkpreview.resolve` WS-RPC kind and
// re-emits the block [`Resolved`](PreviewState::Resolved) / [`Failed`](PreviewState::Failed)
// — the **same lazy-resolve→rebuild-document pattern** feed already uses for
// `media_hash` and the quoted-post fallback (new capability, established
// mechanism). Not produced by any P2/P3 snapshot projection; only the markdown
// body producer emits it. The preview image
// ([`PreviewState::Resolved`]`::image_hash`) is **blocked-by-default** exactly
// like any [`RemoteImage`](Self::RemoteImage) (render-model.md § D3 posture,
// user-ratified 2026-06-27): its [`revealed`](PreviewState::Resolved::revealed)
// flag is projected from the post's reveal set by the manager (the D3 twin) and
// drives [`has_blocked_remote_images`](RenderDocument::has_blocked_remote_images),
// so the post's `load-remote-content-button` covers the og:image too; the
// per-app card paints the image only when `revealed`.
type RenderBlockLinkPreview struct {
	Url   string
	State PreviewState
}

func (e RenderBlockLinkPreview) Destroy() {
	FfiDestroyerString{}.Destroy(e.Url)
	FfiDestroyerPreviewState{}.Destroy(e.State)
}

// A first-class quoted-post node — the typed, in-body-order projection of
// `fauna_feed::QuotedPostView` (render-model.md § D2/D6: a feed quote-post is
// a **block**, not a sibling snapshot field). A **strict superset** of that
// view's fields (priority #4) so no render data is lost: `post_id` is the hex
// id the card links to, `author` the hex author of the quoted post, `body` the
// already-truncated (280-char cap) quoted body, and `verification` whether
// **this client** cryptographically verified the *quoted* post's signed
// envelope (carried from [`QuotedPostView::verification`](fauna_feed::QuotedPostView)
// so the quoted-embed card paints the "unverified source" badge **iff**
// [`Failed`](VerificationStatus::Failed) — `security.md` § Client display of
// unverified content). Not produced by the markdown path; the feed manager
// folds one in **after** the body when `resolve_quoted_post` resolves the
// quote (lazy-resolve→rebuild-document), so every app walks it instead of
// reading the sibling `quoted_post_id` field and re-projecting the quote itself.
//
// `legal_takedown_ref` is `Some(reference)` when the quoted post has been
// **taken down under a legal obligation** (`moderation.md` § Categories &
// enforcement item 1): the nest withholds its body, so `body`/`author` are
// empty and the client renders the shared tombstone
// (`fauna_core::obligation::legal_takedown_tombstone(reference)` — "Removed
// under legal obligation ({reference})") in place of the quoted content,
// never a blank/broken embed. Additive `Option` (`None` for every normal
// quote), carried from [`QuotedPostView::legal_takedown_ref`](fauna_feed::QuotedPostView)
// through [`build_post_document`](fauna_feed::build_post_document).
type RenderBlockQuotedPost struct {
	PostId           string
	Author           string
	Body             string
	Verification     VerificationStatus
	AuthoringOrigin  AuthoringOriginStatus
	LegalTakedownRef *string
	NotFound         bool
}

func (e RenderBlockQuotedPost) Destroy() {
	FfiDestroyerString{}.Destroy(e.PostId)
	FfiDestroyerString{}.Destroy(e.Author)
	FfiDestroyerString{}.Destroy(e.Body)
	FfiDestroyerVerificationStatus{}.Destroy(e.Verification)
	FfiDestroyerAuthoringOriginStatus{}.Destroy(e.AuthoringOrigin)
	FfiDestroyerOptionalString{}.Destroy(e.LegalTakedownRef)
	FfiDestroyerBool{}.Destroy(e.NotFound)
}

// A first-class in-bubble reply-quote node — the conversations analogue of
// [`QuotedPost`](Self::QuotedPost) (render-model.md § D2: a reply-quote is a
// **block**, not a sibling `reply_to` field every bubble re-projects). The
// conversations manager folds one in at read time
// (`ConversationsManager::thread_detail`, beside the D3 reveal projection)
// when a message replies to a parent loaded in the **same thread**,
// **prepended** as the first block so the in-order client walkers paint it
// above the body; hidden (no block) when the parent isn't loaded. Carries the
// parent's `author_display` and a plaintext `snippet` of the parent body
// (clients clamp the snippet to ≤ 2 lines — value lives in `fauna_conversations`).
// Not produced by the markdown path.
type RenderBlockQuotedMessage struct {
	AuthorDisplay string
	Snippet       string
}

func (e RenderBlockQuotedMessage) Destroy() {
	FfiDestroyerString{}.Destroy(e.AuthorDisplay)
	FfiDestroyerString{}.Destroy(e.Snippet)
}

type FfiConverterRenderBlock struct{}

var FfiConverterRenderBlockINSTANCE = FfiConverterRenderBlock{}

func (c FfiConverterRenderBlock) Lift(rb RustBufferI) RenderBlock {
	return LiftFromRustBuffer[RenderBlock](c, rb)
}

func (c FfiConverterRenderBlock) Lower(value RenderBlock) C.RustBuffer {
	return LowerIntoRustBuffer[RenderBlock](c, value)
}

func (c FfiConverterRenderBlock) LowerExternal(value RenderBlock) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RenderBlock](c, value))
}
func (FfiConverterRenderBlock) Read(reader io.Reader) RenderBlock {
	id := readInt32(reader)
	switch id {
	case 1:
		return RenderBlockParagraph{
			FfiConverterSequenceInlineINSTANCE.Read(reader),
		}
	case 2:
		return RenderBlockHeading{
			FfiConverterUint8INSTANCE.Read(reader),
			FfiConverterSequenceInlineINSTANCE.Read(reader),
		}
	case 3:
		return RenderBlockListBlock{
			FfiConverterBoolINSTANCE.Read(reader),
			FfiConverterSequenceRenderDocumentINSTANCE.Read(reader),
		}
	case 4:
		return RenderBlockTaskList{
			FfiConverterSequenceTaskItemINSTANCE.Read(reader),
		}
	case 5:
		return RenderBlockCodeBlock{
			FfiConverterOptionalStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 6:
		return RenderBlockBlockQuote{
			FfiConverterSequenceRenderBlockINSTANCE.Read(reader),
		}
	case 7:
		return RenderBlockImage{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 8:
		return RenderBlockVideo{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 9:
		return RenderBlockProxiedImage{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 10:
		return RenderBlockProxiedVideo{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 11:
		return RenderBlockRemoteImage{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterBoolINSTANCE.Read(reader),
		}
	case 12:
		return RenderBlockAttachment{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterUint64INSTANCE.Read(reader),
			FfiConverterBoolINSTANCE.Read(reader),
			FfiConverterBoolINSTANCE.Read(reader),
		}
	case 13:
		return RenderBlockLinkPreview{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterPreviewStateINSTANCE.Read(reader),
		}
	case 14:
		return RenderBlockQuotedPost{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterVerificationStatusINSTANCE.Read(reader),
			FfiConverterAuthoringOriginStatusINSTANCE.Read(reader),
			FfiConverterOptionalStringINSTANCE.Read(reader),
			FfiConverterBoolINSTANCE.Read(reader),
		}
	case 15:
		return RenderBlockQuotedMessage{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterRenderBlock.Read()", id))
	}
}

func (FfiConverterRenderBlock) Write(writer io.Writer, value RenderBlock) {
	switch variant_value := value.(type) {
	case RenderBlockParagraph:
		writeInt32(writer, 1)
		FfiConverterSequenceInlineINSTANCE.Write(writer, variant_value.Inlines)
	case RenderBlockHeading:
		writeInt32(writer, 2)
		FfiConverterUint8INSTANCE.Write(writer, variant_value.Level)
		FfiConverterSequenceInlineINSTANCE.Write(writer, variant_value.Inlines)
	case RenderBlockListBlock:
		writeInt32(writer, 3)
		FfiConverterBoolINSTANCE.Write(writer, variant_value.Ordered)
		FfiConverterSequenceRenderDocumentINSTANCE.Write(writer, variant_value.Items)
	case RenderBlockTaskList:
		writeInt32(writer, 4)
		FfiConverterSequenceTaskItemINSTANCE.Write(writer, variant_value.Items)
	case RenderBlockCodeBlock:
		writeInt32(writer, 5)
		FfiConverterOptionalStringINSTANCE.Write(writer, variant_value.Lang)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Text)
	case RenderBlockBlockQuote:
		writeInt32(writer, 6)
		FfiConverterSequenceRenderBlockINSTANCE.Write(writer, variant_value.Blocks)
	case RenderBlockImage:
		writeInt32(writer, 7)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Hash)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Alt)
	case RenderBlockVideo:
		writeInt32(writer, 8)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Hash)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Alt)
	case RenderBlockProxiedImage:
		writeInt32(writer, 9)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Path)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Alt)
	case RenderBlockProxiedVideo:
		writeInt32(writer, 10)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Path)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Alt)
	case RenderBlockRemoteImage:
		writeInt32(writer, 11)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Url)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Alt)
		FfiConverterBoolINSTANCE.Write(writer, variant_value.Revealed)
	case RenderBlockAttachment:
		writeInt32(writer, 12)
		FfiConverterStringINSTANCE.Write(writer, variant_value.BlobHash)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Filename)
		FfiConverterStringINSTANCE.Write(writer, variant_value.MimeType)
		FfiConverterUint64INSTANCE.Write(writer, variant_value.SizeBytes)
		FfiConverterBoolINSTANCE.Write(writer, variant_value.IsImage)
		FfiConverterBoolINSTANCE.Write(writer, variant_value.C2pa)
	case RenderBlockLinkPreview:
		writeInt32(writer, 13)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Url)
		FfiConverterPreviewStateINSTANCE.Write(writer, variant_value.State)
	case RenderBlockQuotedPost:
		writeInt32(writer, 14)
		FfiConverterStringINSTANCE.Write(writer, variant_value.PostId)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Author)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Body)
		FfiConverterVerificationStatusINSTANCE.Write(writer, variant_value.Verification)
		FfiConverterAuthoringOriginStatusINSTANCE.Write(writer, variant_value.AuthoringOrigin)
		FfiConverterOptionalStringINSTANCE.Write(writer, variant_value.LegalTakedownRef)
		FfiConverterBoolINSTANCE.Write(writer, variant_value.NotFound)
	case RenderBlockQuotedMessage:
		writeInt32(writer, 15)
		FfiConverterStringINSTANCE.Write(writer, variant_value.AuthorDisplay)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Snippet)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterRenderBlock.Write", value))
	}
}

type FfiDestroyerRenderBlock struct{}

func (_ FfiDestroyerRenderBlock) Destroy(value RenderBlock) {
	value.Destroy()
}

// What a user can actually answer — the submission subset of [`RsvpState`].
//
// `Tentative`, `Invited` and `Waitlisted` are deliberately absent: they are
// inbound/roster states, not answers a Fauna app offers
// (`caldav-server.md` § RSVP semantics). Submitting one is now unrepresentable
// rather than silently becoming `NEEDS-ACTION`.
type RsvpResponse uint

const (
	RsvpResponseGoing      RsvpResponse = 1
	RsvpResponseInterested RsvpResponse = 2
	RsvpResponseDeclined   RsvpResponse = 3
)

type FfiConverterRsvpResponse struct{}

var FfiConverterRsvpResponseINSTANCE = FfiConverterRsvpResponse{}

func (c FfiConverterRsvpResponse) Lift(rb RustBufferI) RsvpResponse {
	return LiftFromRustBuffer[RsvpResponse](c, rb)
}

func (c FfiConverterRsvpResponse) Lower(value RsvpResponse) C.RustBuffer {
	return LowerIntoRustBuffer[RsvpResponse](c, value)
}

func (c FfiConverterRsvpResponse) LowerExternal(value RsvpResponse) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RsvpResponse](c, value))
}
func (FfiConverterRsvpResponse) Read(reader io.Reader) RsvpResponse {
	id := readInt32(reader)
	return RsvpResponse(id)
}

func (FfiConverterRsvpResponse) Write(writer io.Writer, value RsvpResponse) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerRsvpResponse struct{}

func (_ FfiDestroyerRsvpResponse) Destroy(value RsvpResponse) {
}

// An attendee's RSVP status — the full closed set, including the three states
// no user submits (see the module docs).
//
// The `snake_case` serde repr is the Fauna wire/status string
// (`"going"` / `"interested"` / `"tentative"` / `"declined"` / `"invited"` /
// `"waitlisted"`), which is what `AttendeeInfo::fauna_status` holds and what
// every app compares against today.
type RsvpState uint

const (
	// `PARTSTAT=ACCEPTED`.
	RsvpStateGoing RsvpState = 1
	// Fauna-native "soft yes". Projects to `PARTSTAT=TENTATIVE`; recoverable
	// only from the sidecar.
	RsvpStateInterested RsvpState = 2
	// A stock client's own `PARTSTAT=TENTATIVE`, with no sidecar marker.
	RsvpStateTentative RsvpState = 3
	// `PARTSTAT=DECLINED`.
	RsvpStateDeclined RsvpState = 4
	// The initial roster state — `PARTSTAT=NEEDS-ACTION`, nobody has answered.
	//
	// Named for the Fauna status string every app already compares against
	// (`"invited"`), not for the iCalendar spelling of the `PARTSTAT` it
	// projects to. A variant named `NeedsAction` whose `as_str` were
	// `"invited"` would be a permanent trap for the next reader.
	RsvpStateInvited RsvpState = 5
	// Fauna-native, carried as the `X-FAUNA-STATUS=waitlisted` attendee param
	// beside `PARTSTAT=NEEDS-ACTION` (`crate::ical` emits and parses it).
	RsvpStateWaitlisted RsvpState = 6
)

type FfiConverterRsvpState struct{}

var FfiConverterRsvpStateINSTANCE = FfiConverterRsvpState{}

func (c FfiConverterRsvpState) Lift(rb RustBufferI) RsvpState {
	return LiftFromRustBuffer[RsvpState](c, rb)
}

func (c FfiConverterRsvpState) Lower(value RsvpState) C.RustBuffer {
	return LowerIntoRustBuffer[RsvpState](c, value)
}

func (c FfiConverterRsvpState) LowerExternal(value RsvpState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RsvpState](c, value))
}
func (FfiConverterRsvpState) Read(reader io.Reader) RsvpState {
	id := readInt32(reader)
	return RsvpState(id)
}

func (FfiConverterRsvpState) Write(writer io.Writer, value RsvpState) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerRsvpState struct{}

func (_ FfiDestroyerRsvpState) Destroy(value RsvpState) {
}

// The concept a source/protocol icon depicts. A stable semantic token; the
// only icon code that stays per-app is the *call site* that asks
// [`SourceGlyph::emoji`] for the glyph and hands it to the native widget.
//
// Serializes to its lowercase id (`"fox"`, `"butterfly"`, …) so web reads the
// same key off the serialized conversations snapshot and the feed badge;
// native apps switch on the `uniffi::Enum` directly.
//
// `Archive` sits after `Unknown`, out of the source-family grouping above,
// by design: UniFFI numbers variants in declaration order and the Go
// binding is hand-mirrored, so a new concept is appended at the end rather
// than inserted where it reads best (see
// `archive_is_last_so_existing_ffi_discriminants_are_stable` below).
type SourceGlyph uint

const (
	// The native Fauna protocol — a fox (the project's animal brand).
	SourceGlyphFox SourceGlyph = 1
	// Email / SMTP — an envelope.
	SourceGlyphEnvelope SourceGlyph = 2
	// Bluesky / the AT Protocol — a butterfly (Bluesky's brand mark).
	SourceGlyphButterfly SourceGlyph = 3
	// Nostr — a lightning bolt.
	SourceGlyphBolt SourceGlyph = 4
	// The Fediverse (ActivityPub: Mastodon / Pleroma / Misskey / …) — a globe.
	// Deliberately the *generic* fediverse concept, not the Mastodon elephant,
	// since the source is broader than Mastodon.
	SourceGlyphGlobe SourceGlyph = 5
	// A source outside the known set — a generic broadcast / antenna concept.
	SourceGlyphUnknown SourceGlyph = 6
	// Content re-authored from an export archive (a Facebook or Instagram
	// import, `behavior/archive-import.md`) — a box. The feed badge of an
	// imported post and the `Archive` conversations rail both resolve here;
	// the badge label names the platform.
	SourceGlyphArchive SourceGlyph = 7
	// A bridge — the generic concept for a conversation or post carried by a
	// bridge principal (`ui/conversations.md` § Where logic lives → *The
	// `Bridged` adapter*): what `Rail::Bridged` paints when the thread carries
	// no bridge identity, and a glyph a bridge manifest may declare
	// (`architecture/third-party.md` § The manifest → *The `bridge` block*).
	// Appended last, like `Archive`, so existing FFI discriminants stand.
	SourceGlyphBridge SourceGlyph = 8
)

type FfiConverterSourceGlyph struct{}

var FfiConverterSourceGlyphINSTANCE = FfiConverterSourceGlyph{}

func (c FfiConverterSourceGlyph) Lift(rb RustBufferI) SourceGlyph {
	return LiftFromRustBuffer[SourceGlyph](c, rb)
}

func (c FfiConverterSourceGlyph) Lower(value SourceGlyph) C.RustBuffer {
	return LowerIntoRustBuffer[SourceGlyph](c, value)
}

func (c FfiConverterSourceGlyph) LowerExternal(value SourceGlyph) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[SourceGlyph](c, value))
}
func (FfiConverterSourceGlyph) Read(reader io.Reader) SourceGlyph {
	id := readInt32(reader)
	return SourceGlyph(id)
}

func (FfiConverterSourceGlyph) Write(writer io.Writer, value SourceGlyph) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerSourceGlyph struct{}

func (_ FfiDestroyerSourceGlyph) Destroy(value SourceGlyph) {
}

type SpfVerdict uint

const (
	SpfVerdictNone      SpfVerdict = 1
	SpfVerdictPass      SpfVerdict = 2
	SpfVerdictFail      SpfVerdict = 3
	SpfVerdictSoftFail  SpfVerdict = 4
	SpfVerdictNeutral   SpfVerdict = 5
	SpfVerdictPermError SpfVerdict = 6
	SpfVerdictTempError SpfVerdict = 7
)

type FfiConverterSpfVerdict struct{}

var FfiConverterSpfVerdictINSTANCE = FfiConverterSpfVerdict{}

func (c FfiConverterSpfVerdict) Lift(rb RustBufferI) SpfVerdict {
	return LiftFromRustBuffer[SpfVerdict](c, rb)
}

func (c FfiConverterSpfVerdict) Lower(value SpfVerdict) C.RustBuffer {
	return LowerIntoRustBuffer[SpfVerdict](c, value)
}

func (c FfiConverterSpfVerdict) LowerExternal(value SpfVerdict) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[SpfVerdict](c, value))
}
func (FfiConverterSpfVerdict) Read(reader io.Reader) SpfVerdict {
	id := readInt32(reader)
	return SpfVerdict(id)
}

func (FfiConverterSpfVerdict) Write(writer io.Writer, value SpfVerdict) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerSpfVerdict struct{}

func (_ FfiDestroyerSpfVerdict) Destroy(value SpfVerdict) {
}

// The shared **six-state** per-file sync-status display vocabulary — what the
// `sync-state-badge` on the media page tells the user about where a file lives
// (file-sync.md § Per-file sync-status display). Each desktop-engine client
// (apple `SyncEngine`+SwiftData, android Room, the Windows cfapi host) derives
// the badge from its own local engine and converges the **label text** here;
// the badge icon/color stays an idiomatic per-app render (the same
// render-vs-label split as [`offer_status_label`] / [`contact_status_label`]).
// Control-plane clients (web, the Linux media page) have no local engine and
// render only `Synced`. The engine-internal eight-variant
// `fauna_sync_engine::SyncState` collapses onto these six via
// `SyncState::to_display()` (engine-holding clients only — today Windows).
type SyncDisplayState uint

const (
	// Present and up to date both locally and on the nest.
	SyncDisplayStateSynced SyncDisplayState = 1
	// Present on this device, not yet uploaded.
	SyncDisplayStateLocalOnly SyncDisplayState = 2
	// On the nest only — a placeholder on this device (on-demand), not hydrated.
	SyncDisplayStateRemoteOnly SyncDisplayState = 3
	// Local changes are being pushed.
	SyncDisplayStateUploading SyncDisplayState = 4
	// Remote bytes are being pulled / hydrated.
	SyncDisplayStateDownloading SyncDisplayState = 5
	// Local and remote diverged; resolve on the Peers/Devices page.
	SyncDisplayStateConflict SyncDisplayState = 6
)

type FfiConverterSyncDisplayState struct{}

var FfiConverterSyncDisplayStateINSTANCE = FfiConverterSyncDisplayState{}

func (c FfiConverterSyncDisplayState) Lift(rb RustBufferI) SyncDisplayState {
	return LiftFromRustBuffer[SyncDisplayState](c, rb)
}

func (c FfiConverterSyncDisplayState) Lower(value SyncDisplayState) C.RustBuffer {
	return LowerIntoRustBuffer[SyncDisplayState](c, value)
}

func (c FfiConverterSyncDisplayState) LowerExternal(value SyncDisplayState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[SyncDisplayState](c, value))
}
func (FfiConverterSyncDisplayState) Read(reader io.Reader) SyncDisplayState {
	id := readInt32(reader)
	return SyncDisplayState(id)
}

func (FfiConverterSyncDisplayState) Write(writer io.Writer, value SyncDisplayState) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerSyncDisplayState struct{}

func (_ FfiDestroyerSyncDisplayState) Destroy(value SyncDisplayState) {
}

// Whether a rendered post/quote's signed envelope was cryptographically
// verified by *this client* (`security.md` § Client display of unverified
// content; review findings F-CL2/F-CL3). Three honest states, **not** a bool —
// a bool conflates "we never checked" with "we checked and it's authentic",
// which is exactly the dangerous conflation the unverified-source indicator
// exists to surface (an unverified projection must never *look* verified).
//
// The indicator (a muted "unverified source" badge) renders **iff** [`Failed`]
// — the DKIM-fail analogue. [`Unchecked`] is the normal feed-list card and
// carries **no** badge: a `FeedPostItem` is the home nest's trusted index
// projection (it has no envelope to verify — see
// `fauna_feed::PostSummary`), and the home-nest trust model
// (`security.md` § Transport trust) accepts it. Verification can only run where
// the client decodes a **raw signed envelope** (`fauna.posts.get` + `decode_post`:
// post-detail, the quoted-post fallback, media-resolve), so most list cards
// stay [`Unchecked`] until the post is opened/decoded. A [`Failed`] post still
// renders in full — a transient key-rotation-lag false-negative must not make a
// legitimate post silently vanish (`security.md` § Client display).
//
// [`Unchecked`]: VerificationStatus::Unchecked
// [`Failed`]: VerificationStatus::Failed
type VerificationStatus uint

const (
	// Not independently verified by this client — the default for a nest-index
	// projection (`FeedPostItem` carries no envelope). **No badge.** This is the
	// trusted-home-nest case, not a failure.
	VerificationStatusUnchecked VerificationStatus = 1
	// The client decoded the raw signed envelope and the BLAKE3-CID + Ed25519
	// signature check **passed** (`decode_post` → `valid == true`). **No badge.**
	VerificationStatusVerified VerificationStatus = 2
	// The client decoded the raw signed envelope and verification **failed**
	// (`decode_post` → `valid == false`) — content whose source could not be
	// authenticated. **Renders the "unverified source" badge** while still
	// showing the content.
	VerificationStatusFailed VerificationStatus = 3
)

type FfiConverterVerificationStatus struct{}

var FfiConverterVerificationStatusINSTANCE = FfiConverterVerificationStatus{}

func (c FfiConverterVerificationStatus) Lift(rb RustBufferI) VerificationStatus {
	return LiftFromRustBuffer[VerificationStatus](c, rb)
}

func (c FfiConverterVerificationStatus) Lower(value VerificationStatus) C.RustBuffer {
	return LowerIntoRustBuffer[VerificationStatus](c, value)
}

func (c FfiConverterVerificationStatus) LowerExternal(value VerificationStatus) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[VerificationStatus](c, value))
}
func (FfiConverterVerificationStatus) Read(reader io.Reader) VerificationStatus {
	id := readInt32(reader)
	return VerificationStatus(id)
}

func (FfiConverterVerificationStatus) Write(writer io.Writer, value VerificationStatus) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerVerificationStatus struct{}

func (_ FfiDestroyerVerificationStatus) Destroy(value VerificationStatus) {
}

type FfiConverterOptionalInt64 struct{}

var FfiConverterOptionalInt64INSTANCE = FfiConverterOptionalInt64{}

func (c FfiConverterOptionalInt64) Lift(rb RustBufferI) *int64 {
	return LiftFromRustBuffer[*int64](c, rb)
}

func (_ FfiConverterOptionalInt64) Read(reader io.Reader) *int64 {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterInt64INSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalInt64) Lower(value *int64) C.RustBuffer {
	return LowerIntoRustBuffer[*int64](c, value)
}

func (c FfiConverterOptionalInt64) LowerExternal(value *int64) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*int64](c, value))
}

func (_ FfiConverterOptionalInt64) Write(writer io.Writer, value *int64) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterInt64INSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalInt64 struct{}

func (_ FfiDestroyerOptionalInt64) Destroy(value *int64) {
	if value != nil {
		FfiDestroyerInt64{}.Destroy(*value)
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

func (c FfiConverterOptionalLocalizedText) Lift(rb RustBufferI) *LocalizedText {
	return LiftFromRustBuffer[*LocalizedText](c, rb)
}

func (_ FfiConverterOptionalLocalizedText) Read(reader io.Reader) *LocalizedText {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterLocalizedTextINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalLocalizedText) Lower(value *LocalizedText) C.RustBuffer {
	return LowerIntoRustBuffer[*LocalizedText](c, value)
}

func (c FfiConverterOptionalLocalizedText) LowerExternal(value *LocalizedText) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*LocalizedText](c, value))
}

func (_ FfiConverterOptionalLocalizedText) Write(writer io.Writer, value *LocalizedText) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterLocalizedTextINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalLocalizedText struct{}

func (_ FfiDestroyerOptionalLocalizedText) Destroy(value *LocalizedText) {
	if value != nil {
		FfiDestroyerLocalizedText{}.Destroy(*value)
	}
}

type FfiConverterOptionalRelativeTimeDisplay struct{}

var FfiConverterOptionalRelativeTimeDisplayINSTANCE = FfiConverterOptionalRelativeTimeDisplay{}

func (c FfiConverterOptionalRelativeTimeDisplay) Lift(rb RustBufferI) *RelativeTimeDisplay {
	return LiftFromRustBuffer[*RelativeTimeDisplay](c, rb)
}

func (_ FfiConverterOptionalRelativeTimeDisplay) Read(reader io.Reader) *RelativeTimeDisplay {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterRelativeTimeDisplayINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalRelativeTimeDisplay) Lower(value *RelativeTimeDisplay) C.RustBuffer {
	return LowerIntoRustBuffer[*RelativeTimeDisplay](c, value)
}

func (c FfiConverterOptionalRelativeTimeDisplay) LowerExternal(value *RelativeTimeDisplay) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*RelativeTimeDisplay](c, value))
}

func (_ FfiConverterOptionalRelativeTimeDisplay) Write(writer io.Writer, value *RelativeTimeDisplay) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterRelativeTimeDisplayINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalRelativeTimeDisplay struct{}

func (_ FfiDestroyerOptionalRelativeTimeDisplay) Destroy(value *RelativeTimeDisplay) {
	if value != nil {
		FfiDestroyerRelativeTimeDisplay{}.Destroy(*value)
	}
}

type FfiConverterOptionalRspamdScore struct{}

var FfiConverterOptionalRspamdScoreINSTANCE = FfiConverterOptionalRspamdScore{}

func (c FfiConverterOptionalRspamdScore) Lift(rb RustBufferI) *RspamdScore {
	return LiftFromRustBuffer[*RspamdScore](c, rb)
}

func (_ FfiConverterOptionalRspamdScore) Read(reader io.Reader) *RspamdScore {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterRspamdScoreINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalRspamdScore) Lower(value *RspamdScore) C.RustBuffer {
	return LowerIntoRustBuffer[*RspamdScore](c, value)
}

func (c FfiConverterOptionalRspamdScore) LowerExternal(value *RspamdScore) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*RspamdScore](c, value))
}

func (_ FfiConverterOptionalRspamdScore) Write(writer io.Writer, value *RspamdScore) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterRspamdScoreINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalRspamdScore struct{}

func (_ FfiDestroyerOptionalRspamdScore) Destroy(value *RspamdScore) {
	if value != nil {
		FfiDestroyerRspamdScore{}.Destroy(*value)
	}
}

type FfiConverterSequenceString struct{}

var FfiConverterSequenceStringINSTANCE = FfiConverterSequenceString{}

func (c FfiConverterSequenceString) Lift(rb RustBufferI) []string {
	return LiftFromRustBuffer[[]string](c, rb)
}

func (c FfiConverterSequenceString) Read(reader io.Reader) []string {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]string, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterStringINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceString) Lower(value []string) C.RustBuffer {
	return LowerIntoRustBuffer[[]string](c, value)
}

func (c FfiConverterSequenceString) LowerExternal(value []string) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]string](c, value))
}

func (c FfiConverterSequenceString) Write(writer io.Writer, value []string) {
	if len(value) > math.MaxInt32 {
		panic("[]string is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterStringINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceString struct{}

func (FfiDestroyerSequenceString) Destroy(sequence []string) {
	for _, value := range sequence {
		FfiDestroyerString{}.Destroy(value)
	}
}

type FfiConverterSequenceLocalizedText struct{}

var FfiConverterSequenceLocalizedTextINSTANCE = FfiConverterSequenceLocalizedText{}

func (c FfiConverterSequenceLocalizedText) Lift(rb RustBufferI) []LocalizedText {
	return LiftFromRustBuffer[[]LocalizedText](c, rb)
}

func (c FfiConverterSequenceLocalizedText) Read(reader io.Reader) []LocalizedText {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]LocalizedText, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterLocalizedTextINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceLocalizedText) Lower(value []LocalizedText) C.RustBuffer {
	return LowerIntoRustBuffer[[]LocalizedText](c, value)
}

func (c FfiConverterSequenceLocalizedText) LowerExternal(value []LocalizedText) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]LocalizedText](c, value))
}

func (c FfiConverterSequenceLocalizedText) Write(writer io.Writer, value []LocalizedText) {
	if len(value) > math.MaxInt32 {
		panic("[]LocalizedText is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterLocalizedTextINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceLocalizedText struct{}

func (FfiDestroyerSequenceLocalizedText) Destroy(sequence []LocalizedText) {
	for _, value := range sequence {
		FfiDestroyerLocalizedText{}.Destroy(value)
	}
}

type FfiConverterSequenceRenderDocument struct{}

var FfiConverterSequenceRenderDocumentINSTANCE = FfiConverterSequenceRenderDocument{}

func (c FfiConverterSequenceRenderDocument) Lift(rb RustBufferI) []RenderDocument {
	return LiftFromRustBuffer[[]RenderDocument](c, rb)
}

func (c FfiConverterSequenceRenderDocument) Read(reader io.Reader) []RenderDocument {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]RenderDocument, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterRenderDocumentINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceRenderDocument) Lower(value []RenderDocument) C.RustBuffer {
	return LowerIntoRustBuffer[[]RenderDocument](c, value)
}

func (c FfiConverterSequenceRenderDocument) LowerExternal(value []RenderDocument) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]RenderDocument](c, value))
}

func (c FfiConverterSequenceRenderDocument) Write(writer io.Writer, value []RenderDocument) {
	if len(value) > math.MaxInt32 {
		panic("[]RenderDocument is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterRenderDocumentINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceRenderDocument struct{}

func (FfiDestroyerSequenceRenderDocument) Destroy(sequence []RenderDocument) {
	for _, value := range sequence {
		FfiDestroyerRenderDocument{}.Destroy(value)
	}
}

type FfiConverterSequenceRspamdRuleContribution struct{}

var FfiConverterSequenceRspamdRuleContributionINSTANCE = FfiConverterSequenceRspamdRuleContribution{}

func (c FfiConverterSequenceRspamdRuleContribution) Lift(rb RustBufferI) []RspamdRuleContribution {
	return LiftFromRustBuffer[[]RspamdRuleContribution](c, rb)
}

func (c FfiConverterSequenceRspamdRuleContribution) Read(reader io.Reader) []RspamdRuleContribution {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]RspamdRuleContribution, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterRspamdRuleContributionINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceRspamdRuleContribution) Lower(value []RspamdRuleContribution) C.RustBuffer {
	return LowerIntoRustBuffer[[]RspamdRuleContribution](c, value)
}

func (c FfiConverterSequenceRspamdRuleContribution) LowerExternal(value []RspamdRuleContribution) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]RspamdRuleContribution](c, value))
}

func (c FfiConverterSequenceRspamdRuleContribution) Write(writer io.Writer, value []RspamdRuleContribution) {
	if len(value) > math.MaxInt32 {
		panic("[]RspamdRuleContribution is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterRspamdRuleContributionINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceRspamdRuleContribution struct{}

func (FfiDestroyerSequenceRspamdRuleContribution) Destroy(sequence []RspamdRuleContribution) {
	for _, value := range sequence {
		FfiDestroyerRspamdRuleContribution{}.Destroy(value)
	}
}

type FfiConverterSequenceScoreEntry struct{}

var FfiConverterSequenceScoreEntryINSTANCE = FfiConverterSequenceScoreEntry{}

func (c FfiConverterSequenceScoreEntry) Lift(rb RustBufferI) []ScoreEntry {
	return LiftFromRustBuffer[[]ScoreEntry](c, rb)
}

func (c FfiConverterSequenceScoreEntry) Read(reader io.Reader) []ScoreEntry {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ScoreEntry, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterScoreEntryINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceScoreEntry) Lower(value []ScoreEntry) C.RustBuffer {
	return LowerIntoRustBuffer[[]ScoreEntry](c, value)
}

func (c FfiConverterSequenceScoreEntry) LowerExternal(value []ScoreEntry) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ScoreEntry](c, value))
}

func (c FfiConverterSequenceScoreEntry) Write(writer io.Writer, value []ScoreEntry) {
	if len(value) > math.MaxInt32 {
		panic("[]ScoreEntry is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterScoreEntryINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceScoreEntry struct{}

func (FfiDestroyerSequenceScoreEntry) Destroy(sequence []ScoreEntry) {
	for _, value := range sequence {
		FfiDestroyerScoreEntry{}.Destroy(value)
	}
}

type FfiConverterSequenceTaskItem struct{}

var FfiConverterSequenceTaskItemINSTANCE = FfiConverterSequenceTaskItem{}

func (c FfiConverterSequenceTaskItem) Lift(rb RustBufferI) []TaskItem {
	return LiftFromRustBuffer[[]TaskItem](c, rb)
}

func (c FfiConverterSequenceTaskItem) Read(reader io.Reader) []TaskItem {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]TaskItem, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterTaskItemINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceTaskItem) Lower(value []TaskItem) C.RustBuffer {
	return LowerIntoRustBuffer[[]TaskItem](c, value)
}

func (c FfiConverterSequenceTaskItem) LowerExternal(value []TaskItem) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]TaskItem](c, value))
}

func (c FfiConverterSequenceTaskItem) Write(writer io.Writer, value []TaskItem) {
	if len(value) > math.MaxInt32 {
		panic("[]TaskItem is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterTaskItemINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceTaskItem struct{}

func (FfiDestroyerSequenceTaskItem) Destroy(sequence []TaskItem) {
	for _, value := range sequence {
		FfiDestroyerTaskItem{}.Destroy(value)
	}
}

type FfiConverterSequenceInline struct{}

var FfiConverterSequenceInlineINSTANCE = FfiConverterSequenceInline{}

func (c FfiConverterSequenceInline) Lift(rb RustBufferI) []Inline {
	return LiftFromRustBuffer[[]Inline](c, rb)
}

func (c FfiConverterSequenceInline) Read(reader io.Reader) []Inline {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]Inline, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterInlineINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceInline) Lower(value []Inline) C.RustBuffer {
	return LowerIntoRustBuffer[[]Inline](c, value)
}

func (c FfiConverterSequenceInline) LowerExternal(value []Inline) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]Inline](c, value))
}

func (c FfiConverterSequenceInline) Write(writer io.Writer, value []Inline) {
	if len(value) > math.MaxInt32 {
		panic("[]Inline is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterInlineINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceInline struct{}

func (FfiDestroyerSequenceInline) Destroy(sequence []Inline) {
	for _, value := range sequence {
		FfiDestroyerInline{}.Destroy(value)
	}
}

type FfiConverterSequenceRenderBlock struct{}

var FfiConverterSequenceRenderBlockINSTANCE = FfiConverterSequenceRenderBlock{}

func (c FfiConverterSequenceRenderBlock) Lift(rb RustBufferI) []RenderBlock {
	return LiftFromRustBuffer[[]RenderBlock](c, rb)
}

func (c FfiConverterSequenceRenderBlock) Read(reader io.Reader) []RenderBlock {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]RenderBlock, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterRenderBlockINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceRenderBlock) Lower(value []RenderBlock) C.RustBuffer {
	return LowerIntoRustBuffer[[]RenderBlock](c, value)
}

func (c FfiConverterSequenceRenderBlock) LowerExternal(value []RenderBlock) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]RenderBlock](c, value))
}

func (c FfiConverterSequenceRenderBlock) Write(writer io.Writer, value []RenderBlock) {
	if len(value) > math.MaxInt32 {
		panic("[]RenderBlock is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterRenderBlockINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceRenderBlock struct{}

func (FfiDestroyerSequenceRenderBlock) Destroy(sequence []RenderBlock) {
	for _, value := range sequence {
		FfiDestroyerRenderBlock{}.Destroy(value)
	}
}

type FfiConverterMapStringString struct{}

var FfiConverterMapStringStringINSTANCE = FfiConverterMapStringString{}

func (c FfiConverterMapStringString) Lift(rb RustBufferI) map[string]string {
	return LiftFromRustBuffer[map[string]string](c, rb)
}

func (_ FfiConverterMapStringString) Read(reader io.Reader) map[string]string {
	result := make(map[string]string)
	length := readInt32(reader)
	for i := int32(0); i < length; i++ {
		key := FfiConverterStringINSTANCE.Read(reader)
		value := FfiConverterStringINSTANCE.Read(reader)
		result[key] = value
	}
	return result
}

func (c FfiConverterMapStringString) Lower(value map[string]string) C.RustBuffer {
	return LowerIntoRustBuffer[map[string]string](c, value)
}

func (c FfiConverterMapStringString) LowerExternal(value map[string]string) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[map[string]string](c, value))
}

func (_ FfiConverterMapStringString) Write(writer io.Writer, mapValue map[string]string) {
	if len(mapValue) > math.MaxInt32 {
		panic("map[string]string is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(mapValue)))
	for key, value := range mapValue {
		FfiConverterStringINSTANCE.Write(writer, key)
		FfiConverterStringINSTANCE.Write(writer, value)
	}
}

type FfiDestroyerMapStringString struct{}

func (_ FfiDestroyerMapStringString) Destroy(mapValue map[string]string) {
	for key, value := range mapValue {
		FfiDestroyerString{}.Destroy(key)
		FfiDestroyerString{}.Destroy(value)
	}
}

/**
 * Typealias from the type name used in the UDL file to the builtin type.  This
 * is needed because the UDL type name is used in function/method signatures.
 * It's also what we have an external type that references a custom type.
 */
type SecretBytes = []byte
type FfiConverterTypeSecretBytes = FfiConverterBytes
type FfiDestroyerTypeSecretBytes = FfiDestroyerBytes

var FfiConverterTypeSecretBytesINSTANCE = FfiConverterBytes{}

func LiftFromExternalTypeSecretBytes(value ExternalCRustBuffer) SecretBytes {
	return FfiConverterTypeSecretBytesINSTANCE.Lift(RustBufferFromExternal(value))
}

func LowerToExternalTypeSecretBytes(value SecretBytes) ExternalCRustBuffer {
	return RustBufferFromC(FfiConverterTypeSecretBytesINSTANCE.Lower(value))
}

/**
 * Typealias from the type name used in the UDL file to the builtin type.  This
 * is needed because the UDL type name is used in function/method signatures.
 * It's also what we have an external type that references a custom type.
 */
type SecretString = string
type FfiConverterTypeSecretString = FfiConverterString
type FfiDestroyerTypeSecretString = FfiDestroyerString

var FfiConverterTypeSecretStringINSTANCE = FfiConverterString{}

func LiftFromExternalTypeSecretString(value ExternalCRustBuffer) SecretString {
	return FfiConverterTypeSecretStringINSTANCE.Lift(RustBufferFromExternal(value))
}

func LowerToExternalTypeSecretString(value SecretString) ExternalCRustBuffer {
	return RustBufferFromC(FfiConverterTypeSecretStringINSTANCE.Lower(value))
}

// The built-in mail perimeter's bus rows — the ONE per-kind verdict → row
// mapping (`content-scoring.md` § The scoring-metadata bus, the contract
// phase). The Go MTA calls it over UniFFI at the ingest edge, once per
// recipient (the spam score is per-recipient once the unlisted-recipient
// penalty applies), and sends the rows on `IngestInboundMailRequest.scores`
// beside the per-kind detail fields; the nest stores the rows it is sent and
// derives nothing. The submission twin (a Fauna recipient of a locally
// submitted message) calls it with the same inputs its per-kind fields carry,
// so its bus rows and its detail records agree.
//
// One row per factor that actually produced a verdict: a scorer that did not
// run (rspamd disabled, ClamAV bypass or error, an indeterminate auth verdict)
// emits no row — `spam` always does, since the perimeter spam gate runs for
// every delivery and its `0` is "scored ham". Units: `spam` and `rspamd` are
// milli-points on the 0–15 spam scale (the unit `RspamdScore::scaled_milli`
// already carries); verdict factors map Pass→0, SPF SoftFail→500, a
// definitive Fail→1000, ClamAV Infected→1000 — the crude uniform summary. The
// full detail (signature, rule breakdown, DMARC policy, …) stays in the
// per-kind fields and the columns they populate, which are the detail record
// the bus summarizes, not a shape awaiting retirement.
//
// `spam_score_milli` is the combined perimeter score in milli-points
// (`fauna_mail::spam::combined_spam_score_milli`, penalty applied), NOT the
// floored 0–15 `spam_score` points the wire's per-kind field carries: the
// expand-phase nest-side derivation wrote those points into the per-mille
// row, a unit nothing else on the bus used. The contract phase corrected it
// without a [`scorer_version::SPAM`] bump because no row of the points era
// exists at rest anywhere (`version-compatibility.md` § Dimension 2, the
// 2026-09-24 baseline reset) and no reader consumed the value.
func PerimeterMailScoreRows(spamScoreMilli int32, clamav ClamavVerdict, rspamd *RspamdScore, verdicts AuthVerdicts) []ScoreEntry {
	return FfiConverterSequenceScoreEntryINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_core_fn_func_perimeter_mail_score_rows(FfiConverterInt32INSTANCE.Lower(spamScoreMilli), FfiConverterClamavVerdictINSTANCE.Lower(clamav), FfiConverterOptionalRspamdScoreINSTANCE.Lower(rspamd), FfiConverterAuthVerdictsINSTANCE.Lower(verdicts), _uniffiStatus),
		}
	}))
}

// True iff `url` is safe to hand to the OS/browser's default opener as a
// tier's or unlock-offer's `payment_url` — i.e. it starts with `https://`.
//
// `payment_url` is author-supplied (carried on the wire from a possibly
// compromised/spoofed nest), so it is untrusted the same way the bridge OAuth
// `redirect_url` is: opening it unchecked lets a malicious nest redirect the
// user to an attacker-chosen scheme (the F-CL2 anti-phishing-redirect class; see
// `docs/goal/architecture/security.md`). This is a plain scheme-prefix check,
// not full URL parsing: it matches the check web's
// `$lib/safe-url.ts::isSafeNavUrl` already applies (`parsed.protocol ===
// 'https:'`), and native callers just need the same "https or refuse" gate
// before invoking their platform's opener.
//
// One definition for every app that opens a `payment_url`. Landed 2026-08-10
// after finding real drift: tui duplicated this exact check at two call
// sites instead of sharing it (`feed/mod.rs`, `profile/mod.rs` — now both
// call this fn); linux guarded only the feed-side open
// (`views/feed/post_list.rs`) and was missing it on the profile-side one
// (`views/profile/offers.rs` — both now call this fn); windows had the same
// split (`FeedPage.xaml.cs` guarded, `ProfilePage.xaml.cs` didn't — both now
// call `uniffi.fauna_core.FaunaCoreMethods.IsSafePaymentUrl`); apple's
// `ProfileView.swift::openPayment` had no guard at all (now calls
// `isSafePaymentUrl`). web keeps its own guard (`isSafeNavUrl`) since it
// isn't a UniFFI/wasm consumer of this fn.
func IsSafePaymentUrl(url string) bool {
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_core_fn_func_is_safe_payment_url(FfiConverterStringINSTANCE.Lower(url), _uniffiStatus)
	}))
}
