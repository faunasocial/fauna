package fauna_bridge_atproto

// #include <fauna_bridge_atproto.h>
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
		C.ffi_fauna_bridge_atproto_rustbuffer_free(cb.inner, status)
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
		return C.ffi_fauna_bridge_atproto_rustbuffer_from_bytes(foreign, status)
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
		return C.ffi_fauna_bridge_atproto_uniffi_contract_version()
	})
	if bindingsContractVersion != int(scaffoldingContractVersion) {
		// If this happens try cleaning and rebuilding your project
		panic("fauna_bridge_atproto: UniFFI contract version mismatch")
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_appview_service_did()
		})
		if checksum != 27235 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_appview_service_did: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_authorize()
		})
		if checksum != 33456 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_authorize: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_describe_scope()
		})
		if checksum != 10543 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_describe_scope: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_validate_client_assertion()
		})
		if checksum != 7689 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_validate_client_assertion: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_validate_dpop_proof()
		})
		if checksum != 51465 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_validate_dpop_proof: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_check_fetch_target()
		})
		if checksum != 48848 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_check_fetch_target: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_attach_client_jwks()
		})
		if checksum != 46759 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_attach_client_jwks: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_parse_client_metadata()
		})
		if checksum != 13328 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_parse_client_metadata: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_plan_client_id()
		})
		if checksum != 15211 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_plan_client_id: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_atproto_pds_host()
		})
		if checksum != 39751 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_atproto_pds_host: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_atproto_pds_service_did()
		})
		if checksum != 35193 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_atproto_pds_service_did: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_oauth_issuer()
		})
		if checksum != 4172 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_oauth_issuer: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_oauth_protected_resource_document()
		})
		if checksum != 38963 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_oauth_protected_resource_document: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_finish_par_request()
		})
		if checksum != 43846 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_finish_par_request: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_plan_par_request()
		})
		if checksum != 10628 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_plan_par_request: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_check_grant_expansion()
		})
		if checksum != 54139 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_check_grant_expansion: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_expand_permission_set()
		})
		if checksum != 22399 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_expand_permission_set: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_bridge_atproto_checksum_func_parse_include_scope()
		})
		if checksum != 51 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_bridge_atproto: uniffi_fauna_bridge_atproto_checksum_func_parse_include_scope: UniFFI API checksum mismatch")
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

// A request this server is willing to store.
//
// Deliberately **not** a copy of [`ParRequest`]: the parameters that were
// only ever gates (`response_type`, `code_challenge_method`) are gone,
// because carrying a value whose only legal setting was checked here invites
// a later reader to check it again — differently. What survives is what the
// consent and token slices need.
//
// The client identity is *not* duplicated in here either. Go stores the
// [`ResolvedClient`] beside this, so there is one owner of what the consent
// screen renders; copying the display members into the request would give a
// second, staler answer to the same question.
type AcceptedParRequest struct {
	ClientId    string
	RedirectUri string
	// The grant's **effective** scopes: every entry is one
	// [`scope_grants_something`] answered for, and the base `atproto` scope is
	// present whenever any ATProto-family scope is (an OIDC-only sign-in
	// carries none — see [`plan_par_request`]).
	//
	// A permission set contributes its expanded members here and the
	// `include:` scope itself does **not** appear — the token carries what D8
	// can act on (`atproto-pds-full.md:333`), and a bare `include:` is not
	// that. The set's identity survives in [`Self::sets`], which is what the
	// consent card renders.
	Scopes []string
	// Every permission set this request named, expanded — the frozen
	// expansion (`atproto-pds-full.md:329`).
	//
	// Empty for the overwhelmingly common request that names no sets, which is
	// why it is a list rather than an option: "named none" and "named some
	// that expanded to nothing" are not the same request, and the second one
	// never gets this far (it refuses at PAR).
	Sets  []ExpandedSet
	State string
	// The S256 challenge, carried verbatim for the token endpoint to verify
	// the eventual `code_verifier` against.
	CodeChallenge string
	LoginHint     *string
	// The OIDC `nonce` the request carried, for the ID token to echo.
	Nonce *string
}

func (r *AcceptedParRequest) Destroy() {
	FfiDestroyerString{}.Destroy(r.ClientId)
	FfiDestroyerString{}.Destroy(r.RedirectUri)
	FfiDestroyerSequenceString{}.Destroy(r.Scopes)
	FfiDestroyerSequenceExpandedSet{}.Destroy(r.Sets)
	FfiDestroyerString{}.Destroy(r.State)
	FfiDestroyerString{}.Destroy(r.CodeChallenge)
	FfiDestroyerOptionalString{}.Destroy(r.LoginHint)
	FfiDestroyerOptionalString{}.Destroy(r.Nonce)
}

type FfiConverterAcceptedParRequest struct{}

var FfiConverterAcceptedParRequestINSTANCE = FfiConverterAcceptedParRequest{}

func (c FfiConverterAcceptedParRequest) Lift(rb RustBufferI) AcceptedParRequest {
	return LiftFromRustBuffer[AcceptedParRequest](c, rb)
}

func (c FfiConverterAcceptedParRequest) Read(reader io.Reader) AcceptedParRequest {
	return AcceptedParRequest{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterSequenceExpandedSetINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterAcceptedParRequest) Lower(value AcceptedParRequest) C.RustBuffer {
	return LowerIntoRustBuffer[AcceptedParRequest](c, value)
}

func (c FfiConverterAcceptedParRequest) LowerExternal(value AcceptedParRequest) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AcceptedParRequest](c, value))
}

func (c FfiConverterAcceptedParRequest) Write(writer io.Writer, value AcceptedParRequest) {
	FfiConverterStringINSTANCE.Write(writer, value.ClientId)
	FfiConverterStringINSTANCE.Write(writer, value.RedirectUri)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.Scopes)
	FfiConverterSequenceExpandedSetINSTANCE.Write(writer, value.Sets)
	FfiConverterStringINSTANCE.Write(writer, value.State)
	FfiConverterStringINSTANCE.Write(writer, value.CodeChallenge)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.LoginHint)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Nonce)
}

type FfiDestroyerAcceptedParRequest struct{}

func (_ FfiDestroyerAcceptedParRequest) Destroy(value AcceptedParRequest) {
	value.Destroy()
}

// What the serving process knows: who we are, and what time it is.
//
// `audience` must be **this AS's issuer identifier**, rendered by
// [`crate::oauth_metadata::oauth_issuer`] — the same builder that prints
// `issuer` in the AS document a conformant client read it out of. One owner, so
// the string a correct client puts in `aud` and the string we compare against
// cannot be spelled differently.
type AssertionExpectations struct {
	Audience string
	// Seconds since the Unix epoch, from the serving process's clock.
	NowUnix int64
}

func (r *AssertionExpectations) Destroy() {
	FfiDestroyerString{}.Destroy(r.Audience)
	FfiDestroyerInt64{}.Destroy(r.NowUnix)
}

type FfiConverterAssertionExpectations struct{}

var FfiConverterAssertionExpectationsINSTANCE = FfiConverterAssertionExpectations{}

func (c FfiConverterAssertionExpectations) Lift(rb RustBufferI) AssertionExpectations {
	return LiftFromRustBuffer[AssertionExpectations](c, rb)
}

func (c FfiConverterAssertionExpectations) Read(reader io.Reader) AssertionExpectations {
	return AssertionExpectations{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
	}
}

func (c FfiConverterAssertionExpectations) Lower(value AssertionExpectations) C.RustBuffer {
	return LowerIntoRustBuffer[AssertionExpectations](c, value)
}

func (c FfiConverterAssertionExpectations) LowerExternal(value AssertionExpectations) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AssertionExpectations](c, value))
}

func (c FfiConverterAssertionExpectations) Write(writer io.Writer, value AssertionExpectations) {
	FfiConverterStringINSTANCE.Write(writer, value.Audience)
	FfiConverterInt64INSTANCE.Write(writer, value.NowUnix)
}

type FfiDestroyerAssertionExpectations struct{}

func (_ FfiDestroyerAssertionExpectations) Destroy(value AssertionExpectations) {
	value.Destroy()
}

// An assertion that passed every policy check, decomposed for verification.
type AuthenticatedClient struct {
	// `<header>.<payload>` — the exact ASCII the signature covers.
	SigningInput string
	// The raw ES256 signature, `r || s`, 64 bytes.
	Signature []byte
	// The **selected** key's coordinates. Selection happened here, so Go never
	// chooses which of a client's keys to trust.
	PublicKeyX []byte
	PublicKeyY []byte
	// The assertion's unique identifier, for the caller's replay set.
	Jti string
	// When the replay entry may be forgotten: the assertion's own `exp`, capped
	// at [`ASSERTION_MAX_LIFETIME_SECS`] from now.
	//
	// Deriving the entry's life from the artifact rather than from a store-wide
	// constant is what makes the set's size a function of the cap: an entry is
	// remembered for exactly as long as replaying it could achieve anything,
	// and not one second longer.
	ReplayUntilUnix int64
}

func (r *AuthenticatedClient) Destroy() {
	FfiDestroyerString{}.Destroy(r.SigningInput)
	FfiDestroyerBytes{}.Destroy(r.Signature)
	FfiDestroyerBytes{}.Destroy(r.PublicKeyX)
	FfiDestroyerBytes{}.Destroy(r.PublicKeyY)
	FfiDestroyerString{}.Destroy(r.Jti)
	FfiDestroyerInt64{}.Destroy(r.ReplayUntilUnix)
}

type FfiConverterAuthenticatedClient struct{}

var FfiConverterAuthenticatedClientINSTANCE = FfiConverterAuthenticatedClient{}

func (c FfiConverterAuthenticatedClient) Lift(rb RustBufferI) AuthenticatedClient {
	return LiftFromRustBuffer[AuthenticatedClient](c, rb)
}

func (c FfiConverterAuthenticatedClient) Read(reader io.Reader) AuthenticatedClient {
	return AuthenticatedClient{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
	}
}

func (c FfiConverterAuthenticatedClient) Lower(value AuthenticatedClient) C.RustBuffer {
	return LowerIntoRustBuffer[AuthenticatedClient](c, value)
}

func (c FfiConverterAuthenticatedClient) LowerExternal(value AuthenticatedClient) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AuthenticatedClient](c, value))
}

func (c FfiConverterAuthenticatedClient) Write(writer io.Writer, value AuthenticatedClient) {
	FfiConverterStringINSTANCE.Write(writer, value.SigningInput)
	FfiConverterBytesINSTANCE.Write(writer, value.Signature)
	FfiConverterBytesINSTANCE.Write(writer, value.PublicKeyX)
	FfiConverterBytesINSTANCE.Write(writer, value.PublicKeyY)
	FfiConverterStringINSTANCE.Write(writer, value.Jti)
	FfiConverterInt64INSTANCE.Write(writer, value.ReplayUntilUnix)
}

type FfiDestroyerAuthenticatedClient struct{}

func (_ FfiDestroyerAuthenticatedClient) Destroy(value AuthenticatedClient) {
	value.Destroy()
}

// Everything the authorization decision may consider.
//
// Every field is something the Go frame already has — no new plumbing. The
// string-typed fields (`plane`, `endpoint_class`) are strings *on purpose*:
// an unrecognized value must be representable so the closed-world matrix can
// deny it. A Rust enum here would make "unknown plane denies" unexpressible
// from Go, and therefore dead.
type AuthzInput struct {
	// `xrpc.Caller.Plane` verbatim: [`PLANE_APP_CREDENTIAL`] or [`PLANE_OAUTH`].
	Plane string
	// The scopes the presented credential carries.
	//
	// App plane: exactly one — the access token's `scope` claim. OAuth
	// plane: the grant's granular scopes, with every `include:<NSID>`
	// permission set **already expanded into its member scopes by the
	// caller**. Expansion is frozen at the ceremony, so what arrives here is
	// the grant row's own frozen list and this path never resolves anything.
	//
	// The split behind that (ratified 2026-08-03, § F4 detail's *Permission
	// sets*): Go performs the resolution *fetch* — DNS, DID and the guarded,
	// authenticated record read — and [`crate::permission_set`] performs the
	// *expansion*, since grammar and document interpretation are exactly what
	// belongs in a pure module. Either way this matrix is untouched: members
	// arrive as ordinary granular scopes and [`no_granular_scope_reaches`]
	// binds them by construction.
	Scopes []string
	// The account's external-apps kill-switch, as the bridge last learned it
	// (`fauna.bridges.atproto.sessions_changed`). `false` suspends the
	// account's entire external-app plane.
	ExternalAppsEnabled bool
	// The method being authorized: the route's NSID.
	//
	// `com.atproto.server.getServiceAuth` is checked **twice** — once with
	// `lxm` = the route NSID (may this credential mint at all?) and once with
	// `lxm`/`aud` = the *requested* method and audience (may it mint for
	// that?). Two checks, one decision function; that is what keeps
	// migration-oriented minting deferred-refused without a second code path.
	Lxm string
	// The resolved proxy-target service DID — present only on proxied calls
	// and on the second `getServiceAuth` check. May carry a `#fragment`.
	Aud *string
	// The route's declared `xrpc.EndpointClass`.
	EndpointClass string
}

func (r *AuthzInput) Destroy() {
	FfiDestroyerString{}.Destroy(r.Plane)
	FfiDestroyerSequenceString{}.Destroy(r.Scopes)
	FfiDestroyerBool{}.Destroy(r.ExternalAppsEnabled)
	FfiDestroyerString{}.Destroy(r.Lxm)
	FfiDestroyerOptionalString{}.Destroy(r.Aud)
	FfiDestroyerString{}.Destroy(r.EndpointClass)
}

type FfiConverterAuthzInput struct{}

var FfiConverterAuthzInputINSTANCE = FfiConverterAuthzInput{}

func (c FfiConverterAuthzInput) Lift(rb RustBufferI) AuthzInput {
	return LiftFromRustBuffer[AuthzInput](c, rb)
}

func (c FfiConverterAuthzInput) Read(reader io.Reader) AuthzInput {
	return AuthzInput{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterAuthzInput) Lower(value AuthzInput) C.RustBuffer {
	return LowerIntoRustBuffer[AuthzInput](c, value)
}

func (c FfiConverterAuthzInput) LowerExternal(value AuthzInput) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AuthzInput](c, value))
}

func (c FfiConverterAuthzInput) Write(writer io.Writer, value AuthzInput) {
	FfiConverterStringINSTANCE.Write(writer, value.Plane)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.Scopes)
	FfiConverterBoolINSTANCE.Write(writer, value.ExternalAppsEnabled)
	FfiConverterStringINSTANCE.Write(writer, value.Lxm)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Aud)
	FfiConverterStringINSTANCE.Write(writer, value.EndpointClass)
}

type FfiDestroyerAuthzInput struct{}

func (_ FfiDestroyerAuthzInput) Destroy(value AuthzInput) {
	value.Destroy()
}

// One ES256 verification key from a client's declared key set.
//
// Deliberately *not* a general JWK: this server verifies exactly one algorithm
// (ES256 on P-256 — what the AS document advertises and what ATProto mandates),
// so a key it cannot use is a key it has no reason to carry. Non-EC and
// non-P-256 entries are **skipped** rather than refused, because a real key set
// legitimately holds keys for other purposes; a set with no usable key left
// after skipping is what refuses.
type ClientJwk struct {
	// The key's identifier. `None` for a set that declares a single unnamed
	// key — legal, and the only case in which an assertion may omit `kid`.
	Kid *string
	// Affine coordinates, 32 bytes each, big-endian and zero-padded as JWK
	// requires ([`crate::jws::decode_ec_coordinate`] owns the width rule).
	X []byte
	Y []byte
}

func (r *ClientJwk) Destroy() {
	FfiDestroyerOptionalString{}.Destroy(r.Kid)
	FfiDestroyerBytes{}.Destroy(r.X)
	FfiDestroyerBytes{}.Destroy(r.Y)
}

type FfiConverterClientJwk struct{}

var FfiConverterClientJwkINSTANCE = FfiConverterClientJwk{}

func (c FfiConverterClientJwk) Lift(rb RustBufferI) ClientJwk {
	return LiftFromRustBuffer[ClientJwk](c, rb)
}

func (c FfiConverterClientJwk) Read(reader io.Reader) ClientJwk {
	return ClientJwk{
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
	}
}

func (c FfiConverterClientJwk) Lower(value ClientJwk) C.RustBuffer {
	return LowerIntoRustBuffer[ClientJwk](c, value)
}

func (c FfiConverterClientJwk) LowerExternal(value ClientJwk) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ClientJwk](c, value))
}

func (c FfiConverterClientJwk) Write(writer io.Writer, value ClientJwk) {
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Kid)
	FfiConverterBytesINSTANCE.Write(writer, value.X)
	FfiConverterBytesINSTANCE.Write(writer, value.Y)
}

type FfiDestroyerClientJwk struct{}

func (_ FfiDestroyerClientJwk) Destroy(value ClientJwk) {
	value.Destroy()
}

// What the request says, for the proof to be checked against.
//
// These are facts only the serving process knows (the method it received, the
// URL it is published at, the time it is now), which is why they are inputs
// rather than something this module derives. `htu` in particular must be the
// endpoint URL **this server's own discovery document advertises** — see
// [`crate::oauth_metadata::oauth_par_endpoint_url`].
type DpopExpectations struct {
	// The HTTP method, uppercase.
	Htm string
	// The full request URI with no query and no fragment.
	Htu string
	// Seconds since the Unix epoch, from the serving process's clock.
	NowUnix int64
	// How far in the past an `iat` may be.
	MaxAgeSecs uint32
	// How far in the future an `iat` may be — a client's clock is its own,
	// and a small allowance is the difference between "works" and "works
	// only on well-synchronised machines".
	MaxSkewSecs uint32
	// Which `ath` rule applies (F4 slice 7 — the field that turned the
	// AS-only refusal into a mode).
	//
	// `None` — an **authorization-server** endpoint: no access token
	// accompanies the request, so a proof carrying `ath` was minted for a
	// different request context and is refused.
	//
	// `Some(hash)` — a **resource-server** request: `ath` is *required* and
	// must equal `hash`, the caller-computed `base64url(sha256(access
	// token))` of the token the request actually presented (RFC 9449 §4.3).
	// The caller computes the hash because it is crypto, not policy; it must
	// derive it from the presented token through one owner function, never
	// inline.
	ExpectedAth *string
}

func (r *DpopExpectations) Destroy() {
	FfiDestroyerString{}.Destroy(r.Htm)
	FfiDestroyerString{}.Destroy(r.Htu)
	FfiDestroyerInt64{}.Destroy(r.NowUnix)
	FfiDestroyerUint32{}.Destroy(r.MaxAgeSecs)
	FfiDestroyerUint32{}.Destroy(r.MaxSkewSecs)
	FfiDestroyerOptionalString{}.Destroy(r.ExpectedAth)
}

type FfiConverterDpopExpectations struct{}

var FfiConverterDpopExpectationsINSTANCE = FfiConverterDpopExpectations{}

func (c FfiConverterDpopExpectations) Lift(rb RustBufferI) DpopExpectations {
	return LiftFromRustBuffer[DpopExpectations](c, rb)
}

func (c FfiConverterDpopExpectations) Read(reader io.Reader) DpopExpectations {
	return DpopExpectations{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterDpopExpectations) Lower(value DpopExpectations) C.RustBuffer {
	return LowerIntoRustBuffer[DpopExpectations](c, value)
}

func (c FfiConverterDpopExpectations) LowerExternal(value DpopExpectations) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[DpopExpectations](c, value))
}

func (c FfiConverterDpopExpectations) Write(writer io.Writer, value DpopExpectations) {
	FfiConverterStringINSTANCE.Write(writer, value.Htm)
	FfiConverterStringINSTANCE.Write(writer, value.Htu)
	FfiConverterInt64INSTANCE.Write(writer, value.NowUnix)
	FfiConverterUint32INSTANCE.Write(writer, value.MaxAgeSecs)
	FfiConverterUint32INSTANCE.Write(writer, value.MaxSkewSecs)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.ExpectedAth)
}

type FfiDestroyerDpopExpectations struct{}

func (_ FfiDestroyerDpopExpectations) Destroy(value DpopExpectations) {
	value.Destroy()
}

// A proof that passed every policy check, decomposed for verification.
//
// Holding the pieces rather than the compact string is the point: the caller
// verifies the signature over [`Self::signing_input`] with
// [`Self::public_key_x`]/[`Self::public_key_y`], and there is no path by
// which it could end up verifying a different header than the one judged
// here.
type DpopProof struct {
	// `<header>.<payload>` — the exact ASCII the signature covers.
	SigningInput string
	// The raw ES256 signature, `r || s`, 64 bytes.
	Signature []byte
	// The embedded public key's affine coordinates, 32 bytes each,
	// big-endian and zero-padded as JWK requires.
	PublicKeyX []byte
	PublicKeyY []byte
	// The proof's unique identifier, for the caller's replay set.
	Jti string
	// The server-issued nonce the proof carried, for the caller to check
	// against what it would have issued.
	Nonce string
	// `iat`, carried so a caller can size a replay entry's lifetime from the
	// proof itself rather than from its own arrival time.
	IssuedAt int64
}

func (r *DpopProof) Destroy() {
	FfiDestroyerString{}.Destroy(r.SigningInput)
	FfiDestroyerBytes{}.Destroy(r.Signature)
	FfiDestroyerBytes{}.Destroy(r.PublicKeyX)
	FfiDestroyerBytes{}.Destroy(r.PublicKeyY)
	FfiDestroyerString{}.Destroy(r.Jti)
	FfiDestroyerString{}.Destroy(r.Nonce)
	FfiDestroyerInt64{}.Destroy(r.IssuedAt)
}

type FfiConverterDpopProof struct{}

var FfiConverterDpopProofINSTANCE = FfiConverterDpopProof{}

func (c FfiConverterDpopProof) Lift(rb RustBufferI) DpopProof {
	return LiftFromRustBuffer[DpopProof](c, rb)
}

func (c FfiConverterDpopProof) Read(reader io.Reader) DpopProof {
	return DpopProof{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
	}
}

func (c FfiConverterDpopProof) Lower(value DpopProof) C.RustBuffer {
	return LowerIntoRustBuffer[DpopProof](c, value)
}

func (c FfiConverterDpopProof) LowerExternal(value DpopProof) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[DpopProof](c, value))
}

func (c FfiConverterDpopProof) Write(writer io.Writer, value DpopProof) {
	FfiConverterStringINSTANCE.Write(writer, value.SigningInput)
	FfiConverterBytesINSTANCE.Write(writer, value.Signature)
	FfiConverterBytesINSTANCE.Write(writer, value.PublicKeyX)
	FfiConverterBytesINSTANCE.Write(writer, value.PublicKeyY)
	FfiConverterStringINSTANCE.Write(writer, value.Jti)
	FfiConverterStringINSTANCE.Write(writer, value.Nonce)
	FfiConverterInt64INSTANCE.Write(writer, value.IssuedAt)
}

type FfiDestroyerDpopProof struct{}

func (_ FfiDestroyerDpopProof) Destroy(value DpopProof) {
	value.Destroy()
}

// A resolved permission set, expanded.
type ExpandedSet struct {
	// The set's NSID — the identity the card renders **verbatim**.
	Nsid string
	// The set's human title, **raw**. Attacker-authored: the control-strip
	// fence is machine composition's, not this module's.
	Title *string
	// The set's human description, **raw**. Same fence, same reason.
	Details *string
	// The expansion: ordinary granular scopes, deduplicated, in document
	// order.
	Members []string
	// Every member that contributed nothing, with its reason.
	Ignored []IgnoredMember
}

func (r *ExpandedSet) Destroy() {
	FfiDestroyerString{}.Destroy(r.Nsid)
	FfiDestroyerOptionalString{}.Destroy(r.Title)
	FfiDestroyerOptionalString{}.Destroy(r.Details)
	FfiDestroyerSequenceString{}.Destroy(r.Members)
	FfiDestroyerSequenceIgnoredMember{}.Destroy(r.Ignored)
}

type FfiConverterExpandedSet struct{}

var FfiConverterExpandedSetINSTANCE = FfiConverterExpandedSet{}

func (c FfiConverterExpandedSet) Lift(rb RustBufferI) ExpandedSet {
	return LiftFromRustBuffer[ExpandedSet](c, rb)
}

func (c FfiConverterExpandedSet) Read(reader io.Reader) ExpandedSet {
	return ExpandedSet{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterSequenceIgnoredMemberINSTANCE.Read(reader),
	}
}

func (c FfiConverterExpandedSet) Lower(value ExpandedSet) C.RustBuffer {
	return LowerIntoRustBuffer[ExpandedSet](c, value)
}

func (c FfiConverterExpandedSet) LowerExternal(value ExpandedSet) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ExpandedSet](c, value))
}

func (c FfiConverterExpandedSet) Write(writer io.Writer, value ExpandedSet) {
	FfiConverterStringINSTANCE.Write(writer, value.Nsid)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Title)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Details)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.Members)
	FfiConverterSequenceIgnoredMemberINSTANCE.Write(writer, value.Ignored)
}

type FfiDestroyerExpandedSet struct{}

func (_ FfiDestroyerExpandedSet) Destroy(value ExpandedSet) {
	value.Destroy()
}

// The target of an attacker-directed outbound fetch, as the component that
// will dial it parsed and resolved it.
//
// See the module docs for why this is components + addresses rather than a
// URL string — it is a security property, not a convenience.
type FetchTarget struct {
	// URL scheme as the caller's own parser produced it. Compared
	// case-insensitively; only `https` is allowed.
	Scheme string
	// Host component, with no port. A bracketed IPv6 literal (`[::1]`) is
	// recognized and rejected like any other IP literal.
	Host string
	// Every address the caller resolved `host` to **and will connect to**.
	// The caller must pin these for the connection; re-resolving afterwards
	// re-opens the rebinding window (module docs).
	ResolvedIps []string
}

func (r *FetchTarget) Destroy() {
	FfiDestroyerString{}.Destroy(r.Scheme)
	FfiDestroyerString{}.Destroy(r.Host)
	FfiDestroyerSequenceString{}.Destroy(r.ResolvedIps)
}

type FfiConverterFetchTarget struct{}

var FfiConverterFetchTargetINSTANCE = FfiConverterFetchTarget{}

func (c FfiConverterFetchTarget) Lift(rb RustBufferI) FetchTarget {
	return LiftFromRustBuffer[FetchTarget](c, rb)
}

func (c FfiConverterFetchTarget) Read(reader io.Reader) FetchTarget {
	return FetchTarget{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterFetchTarget) Lower(value FetchTarget) C.RustBuffer {
	return LowerIntoRustBuffer[FetchTarget](c, value)
}

func (c FfiConverterFetchTarget) LowerExternal(value FetchTarget) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[FetchTarget](c, value))
}

func (c FfiConverterFetchTarget) Write(writer io.Writer, value FetchTarget) {
	FfiConverterStringINSTANCE.Write(writer, value.Scheme)
	FfiConverterStringINSTANCE.Write(writer, value.Host)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.ResolvedIps)
}

type FfiDestroyerFetchTarget struct{}

func (_ FfiDestroyerFetchTarget) Destroy(value FetchTarget) {
	value.Destroy()
}

// One member that contributed nothing, and why.
type IgnoredMember struct {
	// The member's zero-based position in the document's `permissions` array,
	// so a diagnostic can point at it without quoting attacker-authored text.
	Index uint32
	// The member's declared resource kind, or the empty string when that is
	// itself what could not be read.
	Kind string
	// Why it was ignored.
	Reason IgnoreReason
	// The offending value, when naming one helps and quoting it is bounded
	// (a resource name, never free-form document text). Empty otherwise.
	Detail string
}

func (r *IgnoredMember) Destroy() {
	FfiDestroyerUint32{}.Destroy(r.Index)
	FfiDestroyerString{}.Destroy(r.Kind)
	FfiDestroyerIgnoreReason{}.Destroy(r.Reason)
	FfiDestroyerString{}.Destroy(r.Detail)
}

type FfiConverterIgnoredMember struct{}

var FfiConverterIgnoredMemberINSTANCE = FfiConverterIgnoredMember{}

func (c FfiConverterIgnoredMember) Lift(rb RustBufferI) IgnoredMember {
	return LiftFromRustBuffer[IgnoredMember](c, rb)
}

func (c FfiConverterIgnoredMember) Read(reader io.Reader) IgnoredMember {
	return IgnoredMember{
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterIgnoreReasonINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterIgnoredMember) Lower(value IgnoredMember) C.RustBuffer {
	return LowerIntoRustBuffer[IgnoredMember](c, value)
}

func (c FfiConverterIgnoredMember) LowerExternal(value IgnoredMember) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[IgnoredMember](c, value))
}

func (c FfiConverterIgnoredMember) Write(writer io.Writer, value IgnoredMember) {
	FfiConverterUint32INSTANCE.Write(writer, value.Index)
	FfiConverterStringINSTANCE.Write(writer, value.Kind)
	FfiConverterIgnoreReasonINSTANCE.Write(writer, value.Reason)
	FfiConverterStringINSTANCE.Write(writer, value.Detail)
}

type FfiDestroyerIgnoredMember struct{}

func (_ FfiDestroyerIgnoredMember) Destroy(value IgnoredMember) {
	value.Destroy()
}

// The pushed authorization request, as the form body presented it.
//
// Every field is a `String` rather than a parsed type for the same reason
// [`crate::authz::AuthzInput`]'s are: an absent or nonsense value must be
// *representable* so this closed-world check can refuse it, and a Go-side
// parse would be Go holding policy.
type ParRequest struct {
	// The `client_id` the request names. Must equal the one the resolved
	// client was resolved from.
	ClientId string
	// Must be `code`.
	ResponseType string
	// Where the authorization response goes. Must match the client's
	// declared set (`crate::oauth_client::redirect_uri_matches`).
	RedirectUri string
	// Space-delimited, as received.
	Scope string
	// The client's CSRF token, echoed back on the redirect. Required: without
	// it the client cannot tie the response to its own request.
	State string
	// PKCE challenge.
	CodeChallenge string
	// Must be `S256`.
	CodeChallengeMethod string
	// Which account the client believes it is authorizing. Carried, never
	// trusted — the consent ceremony resolves the actual account from the
	// authenticated Fauna app that approves, and this only decides which
	// user's app gets the push.
	LoginHint *string
	// OIDC's `nonce` (TP6): an opaque value the client binds its session to,
	// carried from here into the ID token verbatim so the client can tell the
	// token was minted for THIS sign-in (`authorization-server.md` § OIDC
	// (TP6)). Optional, as OIDC Core makes it for the code flow; bounded by
	// [`OIDC_NONCE_MAX_LEN`] because it is stored and echoed.
	Nonce *string
}

func (r *ParRequest) Destroy() {
	FfiDestroyerString{}.Destroy(r.ClientId)
	FfiDestroyerString{}.Destroy(r.ResponseType)
	FfiDestroyerString{}.Destroy(r.RedirectUri)
	FfiDestroyerString{}.Destroy(r.Scope)
	FfiDestroyerString{}.Destroy(r.State)
	FfiDestroyerString{}.Destroy(r.CodeChallenge)
	FfiDestroyerString{}.Destroy(r.CodeChallengeMethod)
	FfiDestroyerOptionalString{}.Destroy(r.LoginHint)
	FfiDestroyerOptionalString{}.Destroy(r.Nonce)
}

type FfiConverterParRequest struct{}

var FfiConverterParRequestINSTANCE = FfiConverterParRequest{}

func (c FfiConverterParRequest) Lift(rb RustBufferI) ParRequest {
	return LiftFromRustBuffer[ParRequest](c, rb)
}

func (c FfiConverterParRequest) Read(reader io.Reader) ParRequest {
	return ParRequest{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterParRequest) Lower(value ParRequest) C.RustBuffer {
	return LowerIntoRustBuffer[ParRequest](c, value)
}

func (c FfiConverterParRequest) LowerExternal(value ParRequest) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ParRequest](c, value))
}

func (c FfiConverterParRequest) Write(writer io.Writer, value ParRequest) {
	FfiConverterStringINSTANCE.Write(writer, value.ClientId)
	FfiConverterStringINSTANCE.Write(writer, value.ResponseType)
	FfiConverterStringINSTANCE.Write(writer, value.RedirectUri)
	FfiConverterStringINSTANCE.Write(writer, value.Scope)
	FfiConverterStringINSTANCE.Write(writer, value.State)
	FfiConverterStringINSTANCE.Write(writer, value.CodeChallenge)
	FfiConverterStringINSTANCE.Write(writer, value.CodeChallengeMethod)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.LoginHint)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Nonce)
}

type FfiDestroyerParRequest struct{}

func (_ FfiDestroyerParRequest) Destroy(value ParRequest) {
	value.Destroy()
}

// A well-formed `include:` scope, split into the set it names and the audience
// the invocation supplies to `inheritAud` members.
type ParsedInclude struct {
	// The permission set's NSID, syntax-valid by construction — this is the
	// only string PS-b may build a resolution chain from.
	Nsid string
	// The `?aud=` parameter, percent-decoded; `None` when the invocation
	// supplied none. Members declaring `inheritAud` take this value.
	Aud *string
}

func (r *ParsedInclude) Destroy() {
	FfiDestroyerString{}.Destroy(r.Nsid)
	FfiDestroyerOptionalString{}.Destroy(r.Aud)
}

type FfiConverterParsedInclude struct{}

var FfiConverterParsedIncludeINSTANCE = FfiConverterParsedInclude{}

func (c FfiConverterParsedInclude) Lift(rb RustBufferI) ParsedInclude {
	return LiftFromRustBuffer[ParsedInclude](c, rb)
}

func (c FfiConverterParsedInclude) Read(reader io.Reader) ParsedInclude {
	return ParsedInclude{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterParsedInclude) Lower(value ParsedInclude) C.RustBuffer {
	return LowerIntoRustBuffer[ParsedInclude](c, value)
}

func (c FfiConverterParsedInclude) LowerExternal(value ParsedInclude) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ParsedInclude](c, value))
}

func (c FfiConverterParsedInclude) Write(writer io.Writer, value ParsedInclude) {
	FfiConverterStringINSTANCE.Write(writer, value.Nsid)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Aud)
}

type FfiDestroyerParsedInclude struct{}

func (_ FfiDestroyerParsedInclude) Destroy(value ParsedInclude) {
	value.Destroy()
}

// A request whose pure half is settled and whose permission sets still have to
// be fetched.
//
// It is deliberately not "a partly-built acceptance": nothing in here may be
// stored, rendered or acted on. It is the argument [`finish_par_request`]
// needs, and the only thing that turns it into a verdict.
type PendingPar struct {
	// Everything decided so far. `scopes` holds the **directly requested**
	// granular scopes only; `sets` is empty until the expansions arrive.
	Request AcceptedParRequest
	// The sets to resolve, in the order the scope string named them — the
	// order [`finish_par_request`] expects the expansions back in.
	Includes []ParsedInclude
}

func (r *PendingPar) Destroy() {
	FfiDestroyerAcceptedParRequest{}.Destroy(r.Request)
	FfiDestroyerSequenceParsedInclude{}.Destroy(r.Includes)
}

type FfiConverterPendingPar struct{}

var FfiConverterPendingParINSTANCE = FfiConverterPendingPar{}

func (c FfiConverterPendingPar) Lift(rb RustBufferI) PendingPar {
	return LiftFromRustBuffer[PendingPar](c, rb)
}

func (c FfiConverterPendingPar) Read(reader io.Reader) PendingPar {
	return PendingPar{
		FfiConverterAcceptedParRequestINSTANCE.Read(reader),
		FfiConverterSequenceParsedIncludeINSTANCE.Read(reader),
	}
}

func (c FfiConverterPendingPar) Lower(value PendingPar) C.RustBuffer {
	return LowerIntoRustBuffer[PendingPar](c, value)
}

func (c FfiConverterPendingPar) LowerExternal(value PendingPar) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[PendingPar](c, value))
}

func (c FfiConverterPendingPar) Write(writer io.Writer, value PendingPar) {
	FfiConverterAcceptedParRequestINSTANCE.Write(writer, value.Request)
	FfiConverterSequenceParsedIncludeINSTANCE.Write(writer, value.Includes)
}

type FfiDestroyerPendingPar struct{}

func (_ FfiDestroyerPendingPar) Destroy(value PendingPar) {
	value.Destroy()
}

// A client identity, as **resolved** — never as asserted.
//
// This is what the consent ceremony renders and what `crate::oauth_par`
// evaluates a request against. It deliberately carries the client's *display*
// members alongside `client_id`, because § F4 detail requires the consent page
// to show the resolved name and logo **beside the requesting origin**.
//
// ⚠ The origin ANCHORS the display members; it is not itself unforgeable —
// state the claim precisely or the next reader inherits a false
// premise. For an **https** client the origin is pinned by
// the fetch: the document must live at that URL and name it back, so a client
// can only present an origin it controls. The **loopback** identity is the
// deliberate exception on BOTH halves: `http://localhost` is claimable by
// anybody by design, and its spelling includes a free-form query whose bytes
// once carried row structure into the consent card. What keeps the anchor
// honest there is [`LOOPBACK_REDIRECT_HOSTS`] (the response can only reach
// the user's own machine) plus [`plan_client_id`]'s control-character refusal
// (the spelling cannot forge the card that renders it).
type ResolvedClient struct {
	// The `client_id` URL, verbatim as the request presented it. The
	// requesting origin the consent screen anchors on.
	ClientId string
	// Display name, as resolved. `None` for a document that omits it — the
	// consent screen then has only the origin, which is the honest state.
	ClientName *string
	// Homepage. Display only.
	ClientUri *string
	// Logo. Display only — **and deliberately not fetched by this server.**
	// Whether and how the approving app loads it is the consent slice's
	// decision, because loading an attacker-named URL inside the user's app
	// discloses the user's address to whoever published the document.
	LogoUri *string
	// Terms of service. Display only.
	TosUri *string
	// Privacy policy. Display only.
	PolicyUri *string
	// Every redirect URI this client may use. Non-empty by construction —
	// a document declaring none is refused.
	RedirectUris []string
	// The scopes the client declares it might request, split from the
	// document's space-delimited `scope` member. A PAR asking for anything
	// outside this set is refused: the document is the client's public
	// commitment, and a grant wider than it would be one the client's own
	// users could not have audited.
	DeclaredScopes []string
	// `true` when the document declares `token_endpoint_auth_method:
	// private_key_jwt` — a confidential client, which must authenticate at
	// the token endpoint.
	//
	// ⚠ **This member is the single owner of "is this client confidential".**
	// The presence of [`Self::jwks`] never implies it and must never be read
	// as implying it: a public client is free to publish keys for reasons of
	// its own, and inferring an authentication method from key material is how
	// a client ends up held to a contract its document never made.
	Confidential bool
	// The client's ES256 signing keys, as its metadata document declares them
	// — populated **only for a confidential client**, because only a
	// confidential client's assertions are ever verified against them.
	//
	// Resolved eagerly and completely: a document declaring `jwks_uri` has
	// already been followed by the time this client exists, so authenticating
	// an assertion is a pure function over data in hand and performs no
	// network I/O at all. See [`attach_client_jwks`] for why that ordering was
	// chosen over fetching lazily at assertion time.
	//
	// Non-empty whenever [`Self::confidential`] is set — a confidential client
	// with no usable key is refused at resolution rather than at the first
	// assertion, where the failure would look like the client's fault.
	Jwks []ClientJwk
	// The `jwks_uri` the document declared, if any — carried so the resolving
	// caller knows there is a second fetch to make, and kept afterwards purely
	// as the provenance of [`Self::jwks`].
	JwksUri *string
	// `true` for the loopback development client. Two behaviours key off it:
	// redirect matching ignores the port, and the display name is this
	// server's, not the client's.
	Loopback bool
	// The document's `fauna` member — the signed kind manifest, a compact JWS
	// carried verbatim (`third-party-kinds.md` § The manifest). `None` for a
	// document without one. Only its *form* is checked here (a string, not a
	// bare object); the signature, the header and the payload are verified
	// by the one shared-Rust door, `fauna_protocol::kind_manifest::verify_manifest`,
	// which the resolving server runs against the document's host before
	// this client is accepted.
	FaunaManifest *string
}

func (r *ResolvedClient) Destroy() {
	FfiDestroyerString{}.Destroy(r.ClientId)
	FfiDestroyerOptionalString{}.Destroy(r.ClientName)
	FfiDestroyerOptionalString{}.Destroy(r.ClientUri)
	FfiDestroyerOptionalString{}.Destroy(r.LogoUri)
	FfiDestroyerOptionalString{}.Destroy(r.TosUri)
	FfiDestroyerOptionalString{}.Destroy(r.PolicyUri)
	FfiDestroyerSequenceString{}.Destroy(r.RedirectUris)
	FfiDestroyerSequenceString{}.Destroy(r.DeclaredScopes)
	FfiDestroyerBool{}.Destroy(r.Confidential)
	FfiDestroyerSequenceClientJwk{}.Destroy(r.Jwks)
	FfiDestroyerOptionalString{}.Destroy(r.JwksUri)
	FfiDestroyerBool{}.Destroy(r.Loopback)
	FfiDestroyerOptionalString{}.Destroy(r.FaunaManifest)
}

type FfiConverterResolvedClient struct{}

var FfiConverterResolvedClientINSTANCE = FfiConverterResolvedClient{}

func (c FfiConverterResolvedClient) Lift(rb RustBufferI) ResolvedClient {
	return LiftFromRustBuffer[ResolvedClient](c, rb)
}

func (c FfiConverterResolvedClient) Read(reader io.Reader) ResolvedClient {
	return ResolvedClient{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterSequenceClientJwkINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterResolvedClient) Lower(value ResolvedClient) C.RustBuffer {
	return LowerIntoRustBuffer[ResolvedClient](c, value)
}

func (c FfiConverterResolvedClient) LowerExternal(value ResolvedClient) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ResolvedClient](c, value))
}

func (c FfiConverterResolvedClient) Write(writer io.Writer, value ResolvedClient) {
	FfiConverterStringINSTANCE.Write(writer, value.ClientId)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.ClientName)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.ClientUri)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.LogoUri)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.TosUri)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.PolicyUri)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.RedirectUris)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.DeclaredScopes)
	FfiConverterBoolINSTANCE.Write(writer, value.Confidential)
	FfiConverterSequenceClientJwkINSTANCE.Write(writer, value.Jwks)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.JwksUri)
	FfiConverterBoolINSTANCE.Write(writer, value.Loopback)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.FaunaManifest)
}

type FfiDestroyerResolvedClient struct{}

func (_ FfiDestroyerResolvedClient) Destroy(value ResolvedClient) {
	value.Destroy()
}

// The decision. `Deny` carries the XRPC error name (which encodes the D6
// sub-type) and the human message; Go maps the name to an HTTP status through
// a mechanical table and emits it verbatim.
type AuthzVerdict interface {
	Destroy()
}
type AuthzVerdictAllow struct {
}

func (e AuthzVerdictAllow) Destroy() {
}

type AuthzVerdictDeny struct {
	XrpcError string
	Message   string
}

func (e AuthzVerdictDeny) Destroy() {
	FfiDestroyerString{}.Destroy(e.XrpcError)
	FfiDestroyerString{}.Destroy(e.Message)
}

type FfiConverterAuthzVerdict struct{}

var FfiConverterAuthzVerdictINSTANCE = FfiConverterAuthzVerdict{}

func (c FfiConverterAuthzVerdict) Lift(rb RustBufferI) AuthzVerdict {
	return LiftFromRustBuffer[AuthzVerdict](c, rb)
}

func (c FfiConverterAuthzVerdict) Lower(value AuthzVerdict) C.RustBuffer {
	return LowerIntoRustBuffer[AuthzVerdict](c, value)
}

func (c FfiConverterAuthzVerdict) LowerExternal(value AuthzVerdict) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AuthzVerdict](c, value))
}
func (FfiConverterAuthzVerdict) Read(reader io.Reader) AuthzVerdict {
	id := readInt32(reader)
	switch id {
	case 1:
		return AuthzVerdictAllow{}
	case 2:
		return AuthzVerdictDeny{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterAuthzVerdict.Read()", id))
	}
}

func (FfiConverterAuthzVerdict) Write(writer io.Writer, value AuthzVerdict) {
	switch variant_value := value.(type) {
	case AuthzVerdictAllow:
		writeInt32(writer, 1)
	case AuthzVerdictDeny:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variant_value.XrpcError)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Message)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterAuthzVerdict.Write", value))
	}
}

type FfiDestroyerAuthzVerdict struct{}

func (_ FfiDestroyerAuthzVerdict) Destroy(value AuthzVerdict) {
	value.Destroy()
}

// The decision. `Invalid` carries an RFC 6749 §5.2 error code and a
// description; the caller maps the code to an HTTP status through the same
// mechanical table every other F4 refusal uses, and never interprets it.
type ClientAssertionVerdict interface {
	Destroy()
}

// The client authenticated — verify the signature and record the `jti`.
type ClientAssertionVerdictAuthenticated struct {
	Client AuthenticatedClient
}

func (e ClientAssertionVerdictAuthenticated) Destroy() {
	FfiDestroyerAuthenticatedClient{}.Destroy(e.Client)
}

// No authentication was required and none was offered.
type ClientAssertionVerdictNotRequired struct {
}

func (e ClientAssertionVerdictNotRequired) Destroy() {
}

type ClientAssertionVerdictInvalid struct {
	Error       string
	Description string
}

func (e ClientAssertionVerdictInvalid) Destroy() {
	FfiDestroyerString{}.Destroy(e.Error)
	FfiDestroyerString{}.Destroy(e.Description)
}

type FfiConverterClientAssertionVerdict struct{}

var FfiConverterClientAssertionVerdictINSTANCE = FfiConverterClientAssertionVerdict{}

func (c FfiConverterClientAssertionVerdict) Lift(rb RustBufferI) ClientAssertionVerdict {
	return LiftFromRustBuffer[ClientAssertionVerdict](c, rb)
}

func (c FfiConverterClientAssertionVerdict) Lower(value ClientAssertionVerdict) C.RustBuffer {
	return LowerIntoRustBuffer[ClientAssertionVerdict](c, value)
}

func (c FfiConverterClientAssertionVerdict) LowerExternal(value ClientAssertionVerdict) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ClientAssertionVerdict](c, value))
}
func (FfiConverterClientAssertionVerdict) Read(reader io.Reader) ClientAssertionVerdict {
	id := readInt32(reader)
	switch id {
	case 1:
		return ClientAssertionVerdictAuthenticated{
			FfiConverterAuthenticatedClientINSTANCE.Read(reader),
		}
	case 2:
		return ClientAssertionVerdictNotRequired{}
	case 3:
		return ClientAssertionVerdictInvalid{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterClientAssertionVerdict.Read()", id))
	}
}

func (FfiConverterClientAssertionVerdict) Write(writer io.Writer, value ClientAssertionVerdict) {
	switch variant_value := value.(type) {
	case ClientAssertionVerdictAuthenticated:
		writeInt32(writer, 1)
		FfiConverterAuthenticatedClientINSTANCE.Write(writer, variant_value.Client)
	case ClientAssertionVerdictNotRequired:
		writeInt32(writer, 2)
	case ClientAssertionVerdictInvalid:
		writeInt32(writer, 3)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Error)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Description)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterClientAssertionVerdict.Write", value))
	}
}

type FfiDestroyerClientAssertionVerdict struct{}

func (_ FfiDestroyerClientAssertionVerdict) Destroy(value ClientAssertionVerdict) {
	value.Destroy()
}

// What Go must do to resolve a `client_id`.
//
// Three outcomes rather than "a URL or an error", because the loopback client
// resolves with **no fetch at all** — folding it into the fetch path would
// mean either fetching `http://localhost` (which the SSRF guard correctly
// refuses) or teaching Go an exception, and Go does not hold policy.
type ClientIdPlan interface {
	Destroy()
}

// GET this URL through `safefetch` — the address checks are
// [`crate::fetch_guard`]'s, not this module's — then hand the body to
// [`parse_client_metadata`] with the same `client_id`.
type ClientIdPlanFetch struct {
	Url string
}

func (e ClientIdPlanFetch) Destroy() {
	FfiDestroyerString{}.Destroy(e.Url)
}

// Already resolved; no network. The loopback development client.
type ClientIdPlanResolved struct {
	Client ResolvedClient
}

func (e ClientIdPlanResolved) Destroy() {
	FfiDestroyerResolvedClient{}.Destroy(e.Client)
}

// Refuse before any I/O.
type ClientIdPlanDeny struct {
	Error       string
	Description string
}

func (e ClientIdPlanDeny) Destroy() {
	FfiDestroyerString{}.Destroy(e.Error)
	FfiDestroyerString{}.Destroy(e.Description)
}

type FfiConverterClientIdPlan struct{}

var FfiConverterClientIdPlanINSTANCE = FfiConverterClientIdPlan{}

func (c FfiConverterClientIdPlan) Lift(rb RustBufferI) ClientIdPlan {
	return LiftFromRustBuffer[ClientIdPlan](c, rb)
}

func (c FfiConverterClientIdPlan) Lower(value ClientIdPlan) C.RustBuffer {
	return LowerIntoRustBuffer[ClientIdPlan](c, value)
}

func (c FfiConverterClientIdPlan) LowerExternal(value ClientIdPlan) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ClientIdPlan](c, value))
}
func (FfiConverterClientIdPlan) Read(reader io.Reader) ClientIdPlan {
	id := readInt32(reader)
	switch id {
	case 1:
		return ClientIdPlanFetch{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 2:
		return ClientIdPlanResolved{
			FfiConverterResolvedClientINSTANCE.Read(reader),
		}
	case 3:
		return ClientIdPlanDeny{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterClientIdPlan.Read()", id))
	}
}

func (FfiConverterClientIdPlan) Write(writer io.Writer, value ClientIdPlan) {
	switch variant_value := value.(type) {
	case ClientIdPlanFetch:
		writeInt32(writer, 1)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Url)
	case ClientIdPlanResolved:
		writeInt32(writer, 2)
		FfiConverterResolvedClientINSTANCE.Write(writer, variant_value.Client)
	case ClientIdPlanDeny:
		writeInt32(writer, 3)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Error)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Description)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterClientIdPlan.Write", value))
	}
}

type FfiDestroyerClientIdPlan struct{}

func (_ FfiDestroyerClientIdPlan) Destroy(value ClientIdPlan) {
	value.Destroy()
}

// The outcome of validating a fetched metadata document.
type ClientResolution interface {
	Destroy()
}
type ClientResolutionResolved struct {
	Client ResolvedClient
}

func (e ClientResolutionResolved) Destroy() {
	FfiDestroyerResolvedClient{}.Destroy(e.Client)
}

type ClientResolutionDeny struct {
	Error       string
	Description string
}

func (e ClientResolutionDeny) Destroy() {
	FfiDestroyerString{}.Destroy(e.Error)
	FfiDestroyerString{}.Destroy(e.Description)
}

type FfiConverterClientResolution struct{}

var FfiConverterClientResolutionINSTANCE = FfiConverterClientResolution{}

func (c FfiConverterClientResolution) Lift(rb RustBufferI) ClientResolution {
	return LiftFromRustBuffer[ClientResolution](c, rb)
}

func (c FfiConverterClientResolution) Lower(value ClientResolution) C.RustBuffer {
	return LowerIntoRustBuffer[ClientResolution](c, value)
}

func (c FfiConverterClientResolution) LowerExternal(value ClientResolution) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ClientResolution](c, value))
}
func (FfiConverterClientResolution) Read(reader io.Reader) ClientResolution {
	id := readInt32(reader)
	switch id {
	case 1:
		return ClientResolutionResolved{
			FfiConverterResolvedClientINSTANCE.Read(reader),
		}
	case 2:
		return ClientResolutionDeny{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterClientResolution.Read()", id))
	}
}

func (FfiConverterClientResolution) Write(writer io.Writer, value ClientResolution) {
	switch variant_value := value.(type) {
	case ClientResolutionResolved:
		writeInt32(writer, 1)
		FfiConverterResolvedClientINSTANCE.Write(writer, variant_value.Client)
	case ClientResolutionDeny:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Error)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Description)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterClientResolution.Write", value))
	}
}

type FfiDestroyerClientResolution struct{}

func (_ FfiDestroyerClientResolution) Destroy(value ClientResolution) {
	value.Destroy()
}

// The decision. `Invalid` carries an RFC 6749 §5.2-style error code and a
// description; the caller maps the code to an HTTP status through the same
// mechanical table every other F4 refusal uses, and never interprets it.
type DpopVerdict interface {
	Destroy()
}
type DpopVerdictValid struct {
	Proof DpopProof
}

func (e DpopVerdictValid) Destroy() {
	FfiDestroyerDpopProof{}.Destroy(e.Proof)
}

type DpopVerdictInvalid struct {
	Error       string
	Description string
}

func (e DpopVerdictInvalid) Destroy() {
	FfiDestroyerString{}.Destroy(e.Error)
	FfiDestroyerString{}.Destroy(e.Description)
}

type FfiConverterDpopVerdict struct{}

var FfiConverterDpopVerdictINSTANCE = FfiConverterDpopVerdict{}

func (c FfiConverterDpopVerdict) Lift(rb RustBufferI) DpopVerdict {
	return LiftFromRustBuffer[DpopVerdict](c, rb)
}

func (c FfiConverterDpopVerdict) Lower(value DpopVerdict) C.RustBuffer {
	return LowerIntoRustBuffer[DpopVerdict](c, value)
}

func (c FfiConverterDpopVerdict) LowerExternal(value DpopVerdict) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[DpopVerdict](c, value))
}
func (FfiConverterDpopVerdict) Read(reader io.Reader) DpopVerdict {
	id := readInt32(reader)
	switch id {
	case 1:
		return DpopVerdictValid{
			FfiConverterDpopProofINSTANCE.Read(reader),
		}
	case 2:
		return DpopVerdictInvalid{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterDpopVerdict.Read()", id))
	}
}

func (FfiConverterDpopVerdict) Write(writer io.Writer, value DpopVerdict) {
	switch variant_value := value.(type) {
	case DpopVerdictValid:
		writeInt32(writer, 1)
		FfiConverterDpopProofINSTANCE.Write(writer, variant_value.Proof)
	case DpopVerdictInvalid:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Error)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Description)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterDpopVerdict.Write", value))
	}
}

type FfiDestroyerDpopVerdict struct{}

func (_ FfiDestroyerDpopVerdict) Destroy(value DpopVerdict) {
	value.Destroy()
}

// The outcome of expanding one set.
type Expansion interface {
	Destroy()
}

// The set expanded. It may still have expanded to **nothing** — an empty
// `members` with the reasons in `ignored`. That is not a refusal here:
// PS-b refuses it at PAR the way a dead scalar scope is refused
// (`atproto-pds-full.md:330`), and it needs the ignore reasons to say why.
type ExpansionExpanded struct {
	Set ExpandedSet
}

func (e ExpansionExpanded) Destroy() {
	FfiDestroyerExpandedSet{}.Destroy(e.Set)
}

// The document could not be used at all.
type ExpansionRefused struct {
	Reason string
}

func (e ExpansionRefused) Destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

type FfiConverterExpansion struct{}

var FfiConverterExpansionINSTANCE = FfiConverterExpansion{}

func (c FfiConverterExpansion) Lift(rb RustBufferI) Expansion {
	return LiftFromRustBuffer[Expansion](c, rb)
}

func (c FfiConverterExpansion) Lower(value Expansion) C.RustBuffer {
	return LowerIntoRustBuffer[Expansion](c, value)
}

func (c FfiConverterExpansion) LowerExternal(value Expansion) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[Expansion](c, value))
}
func (FfiConverterExpansion) Read(reader io.Reader) Expansion {
	id := readInt32(reader)
	switch id {
	case 1:
		return ExpansionExpanded{
			FfiConverterExpandedSetINSTANCE.Read(reader),
		}
	case 2:
		return ExpansionRefused{
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterExpansion.Read()", id))
	}
}

func (FfiConverterExpansion) Write(writer io.Writer, value Expansion) {
	switch variant_value := value.(type) {
	case ExpansionExpanded:
		writeInt32(writer, 1)
		FfiConverterExpandedSetINSTANCE.Write(writer, variant_value.Set)
	case ExpansionRefused:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Reason)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterExpansion.Write", value))
	}
}

type FfiDestroyerExpansion struct{}

func (_ FfiDestroyerExpansion) Destroy(value Expansion) {
	value.Destroy()
}

// The decision. `Deny` carries a stable reason string; each caller maps it to
// its own surface (an XRPC error for the proxy path, an OAuth error for
// client-metadata resolution) — this module does not know about either.
type FetchTargetVerdict interface {
	Destroy()
}
type FetchTargetVerdictAllow struct {
}

func (e FetchTargetVerdictAllow) Destroy() {
}

type FetchTargetVerdictDeny struct {
	Reason string
}

func (e FetchTargetVerdictDeny) Destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

type FfiConverterFetchTargetVerdict struct{}

var FfiConverterFetchTargetVerdictINSTANCE = FfiConverterFetchTargetVerdict{}

func (c FfiConverterFetchTargetVerdict) Lift(rb RustBufferI) FetchTargetVerdict {
	return LiftFromRustBuffer[FetchTargetVerdict](c, rb)
}

func (c FfiConverterFetchTargetVerdict) Lower(value FetchTargetVerdict) C.RustBuffer {
	return LowerIntoRustBuffer[FetchTargetVerdict](c, value)
}

func (c FfiConverterFetchTargetVerdict) LowerExternal(value FetchTargetVerdict) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[FetchTargetVerdict](c, value))
}
func (FfiConverterFetchTargetVerdict) Read(reader io.Reader) FetchTargetVerdict {
	id := readInt32(reader)
	switch id {
	case 1:
		return FetchTargetVerdictAllow{}
	case 2:
		return FetchTargetVerdictDeny{
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterFetchTargetVerdict.Read()", id))
	}
}

func (FfiConverterFetchTargetVerdict) Write(writer io.Writer, value FetchTargetVerdict) {
	switch variant_value := value.(type) {
	case FetchTargetVerdictAllow:
		writeInt32(writer, 1)
	case FetchTargetVerdictDeny:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Reason)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterFetchTargetVerdict.Write", value))
	}
}

type FfiDestroyerFetchTargetVerdict struct{}

func (_ FfiDestroyerFetchTargetVerdict) Destroy(value FetchTargetVerdict) {
	value.Destroy()
}

// Whether one grant's whole expansion fits the caps.
type GrantExpansion interface {
	Destroy()
}

// Within every cap.
type GrantExpansionWithinCaps struct {
}

func (e GrantExpansionWithinCaps) Destroy() {
}

// Over one of them; `reason` rides the PAR `invalid_scope` refusal.
type GrantExpansionRefused struct {
	Reason string
}

func (e GrantExpansionRefused) Destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

type FfiConverterGrantExpansion struct{}

var FfiConverterGrantExpansionINSTANCE = FfiConverterGrantExpansion{}

func (c FfiConverterGrantExpansion) Lift(rb RustBufferI) GrantExpansion {
	return LiftFromRustBuffer[GrantExpansion](c, rb)
}

func (c FfiConverterGrantExpansion) Lower(value GrantExpansion) C.RustBuffer {
	return LowerIntoRustBuffer[GrantExpansion](c, value)
}

func (c FfiConverterGrantExpansion) LowerExternal(value GrantExpansion) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[GrantExpansion](c, value))
}
func (FfiConverterGrantExpansion) Read(reader io.Reader) GrantExpansion {
	id := readInt32(reader)
	switch id {
	case 1:
		return GrantExpansionWithinCaps{}
	case 2:
		return GrantExpansionRefused{
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterGrantExpansion.Read()", id))
	}
}

func (FfiConverterGrantExpansion) Write(writer io.Writer, value GrantExpansion) {
	switch variant_value := value.(type) {
	case GrantExpansionWithinCaps:
		writeInt32(writer, 1)
	case GrantExpansionRefused:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Reason)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterGrantExpansion.Write", value))
	}
}

type FfiDestroyerGrantExpansion struct{}

func (_ FfiDestroyerGrantExpansion) Destroy(value GrantExpansion) {
	value.Destroy()
}

// Why one member of a set contributed nothing.
//
// Every variant is carried out as **data** rather than dropped, because the
// set's author is a third party who cannot see our logs and the client
// developer cannot see the document: a set that expands to less than its
// author intended must be diagnosable from both ends
// (`atproto-pds-full.md:333` — "every ignore surfaced as data for logs and
// diagnostics, never silently").
type IgnoreReason uint

const (
	// The member was not a map at all.
	IgnoreReasonNotAnObject IgnoreReason = 1
	// No readable resource kind — see [`member_kind`] for the two shapes read.
	IgnoreReasonUnknownResourceKind IgnoreReason = 2
	// A member naming another permission set. Depth is 1 by construction, so
	// this is an invalid member, not a recursion to follow.
	IgnoreReasonNestedInclude IgnoreReason = 3
	// A member declaring both `inheritAud` and its own `aud`. The spec's own
	// words: "is invalid (and should be ignored)".
	IgnoreReasonInheritAudWithOwnAud IgnoreReason = 4
	// A member declaring `inheritAud` where the invocation supplied no `?aud=`
	// to inherit. There is nothing to render it into.
	IgnoreReasonInheritAudWithoutInvocationAud IgnoreReason = 5
	// An `rpc` member with no audience from either source.
	IgnoreReasonRpcMemberWithoutAudience IgnoreReason = 6
	// The member named a resource outside the set's own NSID namespace — a
	// sibling group or a parent. This is the constraint that confines a
	// hostile or compromised authority to widening only within its own
	// namespace.
	IgnoreReasonOutsideSetNamespace IgnoreReason = 7
	// The member named a wildcard resource. A wildcard necessarily reaches
	// outside the set's namespace, so no set can express one — called out
	// separately from [`Self::MalformedResourceName`] because a set author who
	// tries it deserves to be told why rather than "that is not an NSID".
	IgnoreReasonWildcardResource IgnoreReason = 8
	// The member's resource name was not a syntactically valid NSID.
	IgnoreReasonMalformedResourceName IgnoreReason = 9
	// The member declared a readable kind but named no resource at all.
	IgnoreReasonNoResourceNamed IgnoreReason = 10
	// The member expanded to a syntactically fine scope that this server's
	// authorization matrix grants nothing under.
	//
	// Recorded by the PAR step rather than by [`expand_permission_set`] — this
	// module knows the `include:` grammar, not what D8 will do with a granular
	// scope — but it belongs in the same list for the same reason every other
	// variant does: a member that reaches neither the token nor the card must
	// be diagnosable, and the alternative is a set whose card is quietly
	// shorter than its document with nothing saying why.
	//
	// ⚠ Its [`IgnoredMember::index`] is the scope's position in
	// [`ExpandedSet::members`], **not** in the document's `permissions` array:
	// expansion deduplicates, so the two stop coinciding, and the coordinate a
	// post-expansion refusal can honestly name is the expansion's. The scope
	// string itself rides `detail`, which is the diagnostic that matters.
	IgnoreReasonNotGrantable IgnoreReason = 11
	// A `repo` member declaring an `action` (or `actions`) restriction. The
	// scope grammar this server evaluates has no action-qualified `repo:` form
	// yet, and the bare `repo:<collection>` the expander would otherwise emit
	// means create, edit *and* delete — so carrying the member would widen it
	// past what its author declared and past what the consent card could say
	// honestly. Fail closed, like any other parameter the expander does not
	// read; widening later (a qualified scope, with `describe_scope` and the
	// matrix learning it in the same change) is additive.
	IgnoreReasonActionRestricted IgnoreReason = 12
)

type FfiConverterIgnoreReason struct{}

var FfiConverterIgnoreReasonINSTANCE = FfiConverterIgnoreReason{}

func (c FfiConverterIgnoreReason) Lift(rb RustBufferI) IgnoreReason {
	return LiftFromRustBuffer[IgnoreReason](c, rb)
}

func (c FfiConverterIgnoreReason) Lower(value IgnoreReason) C.RustBuffer {
	return LowerIntoRustBuffer[IgnoreReason](c, value)
}

func (c FfiConverterIgnoreReason) LowerExternal(value IgnoreReason) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[IgnoreReason](c, value))
}
func (FfiConverterIgnoreReason) Read(reader io.Reader) IgnoreReason {
	id := readInt32(reader)
	return IgnoreReason(id)
}

func (FfiConverterIgnoreReason) Write(writer io.Writer, value IgnoreReason) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerIgnoreReason struct{}

func (_ FfiDestroyerIgnoreReason) Destroy(value IgnoreReason) {
}

// What a scope string turned out to be.
//
// Three outcomes rather than `Option<Result<…>>` because the caller must tell
// "not my business" from "yours and malformed": a scope that is not an
// include falls through to the ordinary grammar, while a malformed include
// **refuses the whole authorization request** (`atproto-pds-full.md:331`).
type IncludeScope interface {
	Destroy()
}

// Not an `include:` scope. Hand it to the ordinary scope grammar.
type IncludeScopeNotAnInclude struct {
}

func (e IncludeScopeNotAnInclude) Destroy() {
}

// A well-formed include, ready to resolve.
type IncludeScopeParsed struct {
	Include ParsedInclude
}

func (e IncludeScopeParsed) Destroy() {
	FfiDestroyerParsedInclude{}.Destroy(e.Include)
}

// An include this server will not act on. `reason` is the diagnostic that
// rides the `invalid_scope` refusal to the client developer.
type IncludeScopeRefused struct {
	Reason string
}

func (e IncludeScopeRefused) Destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

type FfiConverterIncludeScope struct{}

var FfiConverterIncludeScopeINSTANCE = FfiConverterIncludeScope{}

func (c FfiConverterIncludeScope) Lift(rb RustBufferI) IncludeScope {
	return LiftFromRustBuffer[IncludeScope](c, rb)
}

func (c FfiConverterIncludeScope) Lower(value IncludeScope) C.RustBuffer {
	return LowerIntoRustBuffer[IncludeScope](c, value)
}

func (c FfiConverterIncludeScope) LowerExternal(value IncludeScope) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[IncludeScope](c, value))
}
func (FfiConverterIncludeScope) Read(reader io.Reader) IncludeScope {
	id := readInt32(reader)
	switch id {
	case 1:
		return IncludeScopeNotAnInclude{}
	case 2:
		return IncludeScopeParsed{
			FfiConverterParsedIncludeINSTANCE.Read(reader),
		}
	case 3:
		return IncludeScopeRefused{
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterIncludeScope.Read()", id))
	}
}

func (FfiConverterIncludeScope) Write(writer io.Writer, value IncludeScope) {
	switch variant_value := value.(type) {
	case IncludeScopeNotAnInclude:
		writeInt32(writer, 1)
	case IncludeScopeParsed:
		writeInt32(writer, 2)
		FfiConverterParsedIncludeINSTANCE.Write(writer, variant_value.Include)
	case IncludeScopeRefused:
		writeInt32(writer, 3)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Reason)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterIncludeScope.Write", value))
	}
}

type FfiDestroyerIncludeScope struct{}

func (_ FfiDestroyerIncludeScope) Destroy(value IncludeScope) {
	value.Destroy()
}

// What the pure half decided.
type ParPlan interface {
	Destroy()
}

// No permission sets: this request is fully decided, store it.
type ParPlanAccept struct {
	Request AcceptedParRequest
}

func (e ParPlanAccept) Destroy() {
	FfiDestroyerAcceptedParRequest{}.Destroy(e.Request)
}

// Resolve every `pending.includes` entry, expand each, and call
// [`finish_par_request`]. **Nothing may be stored before that returns.**
type ParPlanResolve struct {
	Pending PendingPar
}

func (e ParPlanResolve) Destroy() {
	FfiDestroyerPendingPar{}.Destroy(e.Pending)
}

type ParPlanDeny struct {
	Error       string
	Description string
}

func (e ParPlanDeny) Destroy() {
	FfiDestroyerString{}.Destroy(e.Error)
	FfiDestroyerString{}.Destroy(e.Description)
}

type FfiConverterParPlan struct{}

var FfiConverterParPlanINSTANCE = FfiConverterParPlan{}

func (c FfiConverterParPlan) Lift(rb RustBufferI) ParPlan {
	return LiftFromRustBuffer[ParPlan](c, rb)
}

func (c FfiConverterParPlan) Lower(value ParPlan) C.RustBuffer {
	return LowerIntoRustBuffer[ParPlan](c, value)
}

func (c FfiConverterParPlan) LowerExternal(value ParPlan) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ParPlan](c, value))
}
func (FfiConverterParPlan) Read(reader io.Reader) ParPlan {
	id := readInt32(reader)
	switch id {
	case 1:
		return ParPlanAccept{
			FfiConverterAcceptedParRequestINSTANCE.Read(reader),
		}
	case 2:
		return ParPlanResolve{
			FfiConverterPendingParINSTANCE.Read(reader),
		}
	case 3:
		return ParPlanDeny{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterParPlan.Read()", id))
	}
}

func (FfiConverterParPlan) Write(writer io.Writer, value ParPlan) {
	switch variant_value := value.(type) {
	case ParPlanAccept:
		writeInt32(writer, 1)
		FfiConverterAcceptedParRequestINSTANCE.Write(writer, variant_value.Request)
	case ParPlanResolve:
		writeInt32(writer, 2)
		FfiConverterPendingParINSTANCE.Write(writer, variant_value.Pending)
	case ParPlanDeny:
		writeInt32(writer, 3)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Error)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Description)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterParPlan.Write", value))
	}
}

type FfiDestroyerParPlan struct{}

func (_ FfiDestroyerParPlan) Destroy(value ParPlan) {
	value.Destroy()
}

// The decision. `Deny` carries an RFC 6749 §5.2 error code and a description;
// Go maps the code to an HTTP status through a mechanical table and emits the
// pair as the endpoint's JSON error body.
type ParVerdict interface {
	Destroy()
}
type ParVerdictAccept struct {
	Request AcceptedParRequest
}

func (e ParVerdictAccept) Destroy() {
	FfiDestroyerAcceptedParRequest{}.Destroy(e.Request)
}

type ParVerdictDeny struct {
	Error       string
	Description string
}

func (e ParVerdictDeny) Destroy() {
	FfiDestroyerString{}.Destroy(e.Error)
	FfiDestroyerString{}.Destroy(e.Description)
}

type FfiConverterParVerdict struct{}

var FfiConverterParVerdictINSTANCE = FfiConverterParVerdict{}

func (c FfiConverterParVerdict) Lift(rb RustBufferI) ParVerdict {
	return LiftFromRustBuffer[ParVerdict](c, rb)
}

func (c FfiConverterParVerdict) Lower(value ParVerdict) C.RustBuffer {
	return LowerIntoRustBuffer[ParVerdict](c, value)
}

func (c FfiConverterParVerdict) LowerExternal(value ParVerdict) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ParVerdict](c, value))
}
func (FfiConverterParVerdict) Read(reader io.Reader) ParVerdict {
	id := readInt32(reader)
	switch id {
	case 1:
		return ParVerdictAccept{
			FfiConverterAcceptedParRequestINSTANCE.Read(reader),
		}
	case 2:
		return ParVerdictDeny{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterParVerdict.Read()", id))
	}
}

func (FfiConverterParVerdict) Write(writer io.Writer, value ParVerdict) {
	switch variant_value := value.(type) {
	case ParVerdictAccept:
		writeInt32(writer, 1)
		FfiConverterAcceptedParRequestINSTANCE.Write(writer, variant_value.Request)
	case ParVerdictDeny:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Error)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Description)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterParVerdict.Write", value))
	}
}

type FfiDestroyerParVerdict struct{}

func (_ FfiDestroyerParVerdict) Destroy(value ParVerdict) {
	value.Destroy()
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

type FfiConverterSequenceBytes struct{}

var FfiConverterSequenceBytesINSTANCE = FfiConverterSequenceBytes{}

func (c FfiConverterSequenceBytes) Lift(rb RustBufferI) [][]byte {
	return LiftFromRustBuffer[[][]byte](c, rb)
}

func (c FfiConverterSequenceBytes) Read(reader io.Reader) [][]byte {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([][]byte, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterBytesINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceBytes) Lower(value [][]byte) C.RustBuffer {
	return LowerIntoRustBuffer[[][]byte](c, value)
}

func (c FfiConverterSequenceBytes) LowerExternal(value [][]byte) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[][]byte](c, value))
}

func (c FfiConverterSequenceBytes) Write(writer io.Writer, value [][]byte) {
	if len(value) > math.MaxInt32 {
		panic("[][]byte is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterBytesINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceBytes struct{}

func (FfiDestroyerSequenceBytes) Destroy(sequence [][]byte) {
	for _, value := range sequence {
		FfiDestroyerBytes{}.Destroy(value)
	}
}

type FfiConverterSequenceClientJwk struct{}

var FfiConverterSequenceClientJwkINSTANCE = FfiConverterSequenceClientJwk{}

func (c FfiConverterSequenceClientJwk) Lift(rb RustBufferI) []ClientJwk {
	return LiftFromRustBuffer[[]ClientJwk](c, rb)
}

func (c FfiConverterSequenceClientJwk) Read(reader io.Reader) []ClientJwk {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ClientJwk, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterClientJwkINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceClientJwk) Lower(value []ClientJwk) C.RustBuffer {
	return LowerIntoRustBuffer[[]ClientJwk](c, value)
}

func (c FfiConverterSequenceClientJwk) LowerExternal(value []ClientJwk) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ClientJwk](c, value))
}

func (c FfiConverterSequenceClientJwk) Write(writer io.Writer, value []ClientJwk) {
	if len(value) > math.MaxInt32 {
		panic("[]ClientJwk is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterClientJwkINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceClientJwk struct{}

func (FfiDestroyerSequenceClientJwk) Destroy(sequence []ClientJwk) {
	for _, value := range sequence {
		FfiDestroyerClientJwk{}.Destroy(value)
	}
}

type FfiConverterSequenceExpandedSet struct{}

var FfiConverterSequenceExpandedSetINSTANCE = FfiConverterSequenceExpandedSet{}

func (c FfiConverterSequenceExpandedSet) Lift(rb RustBufferI) []ExpandedSet {
	return LiftFromRustBuffer[[]ExpandedSet](c, rb)
}

func (c FfiConverterSequenceExpandedSet) Read(reader io.Reader) []ExpandedSet {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ExpandedSet, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterExpandedSetINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceExpandedSet) Lower(value []ExpandedSet) C.RustBuffer {
	return LowerIntoRustBuffer[[]ExpandedSet](c, value)
}

func (c FfiConverterSequenceExpandedSet) LowerExternal(value []ExpandedSet) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ExpandedSet](c, value))
}

func (c FfiConverterSequenceExpandedSet) Write(writer io.Writer, value []ExpandedSet) {
	if len(value) > math.MaxInt32 {
		panic("[]ExpandedSet is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterExpandedSetINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceExpandedSet struct{}

func (FfiDestroyerSequenceExpandedSet) Destroy(sequence []ExpandedSet) {
	for _, value := range sequence {
		FfiDestroyerExpandedSet{}.Destroy(value)
	}
}

type FfiConverterSequenceIgnoredMember struct{}

var FfiConverterSequenceIgnoredMemberINSTANCE = FfiConverterSequenceIgnoredMember{}

func (c FfiConverterSequenceIgnoredMember) Lift(rb RustBufferI) []IgnoredMember {
	return LiftFromRustBuffer[[]IgnoredMember](c, rb)
}

func (c FfiConverterSequenceIgnoredMember) Read(reader io.Reader) []IgnoredMember {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]IgnoredMember, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterIgnoredMemberINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceIgnoredMember) Lower(value []IgnoredMember) C.RustBuffer {
	return LowerIntoRustBuffer[[]IgnoredMember](c, value)
}

func (c FfiConverterSequenceIgnoredMember) LowerExternal(value []IgnoredMember) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]IgnoredMember](c, value))
}

func (c FfiConverterSequenceIgnoredMember) Write(writer io.Writer, value []IgnoredMember) {
	if len(value) > math.MaxInt32 {
		panic("[]IgnoredMember is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterIgnoredMemberINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceIgnoredMember struct{}

func (FfiDestroyerSequenceIgnoredMember) Destroy(sequence []IgnoredMember) {
	for _, value := range sequence {
		FfiDestroyerIgnoredMember{}.Destroy(value)
	}
}

type FfiConverterSequenceParsedInclude struct{}

var FfiConverterSequenceParsedIncludeINSTANCE = FfiConverterSequenceParsedInclude{}

func (c FfiConverterSequenceParsedInclude) Lift(rb RustBufferI) []ParsedInclude {
	return LiftFromRustBuffer[[]ParsedInclude](c, rb)
}

func (c FfiConverterSequenceParsedInclude) Read(reader io.Reader) []ParsedInclude {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ParsedInclude, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterParsedIncludeINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceParsedInclude) Lower(value []ParsedInclude) C.RustBuffer {
	return LowerIntoRustBuffer[[]ParsedInclude](c, value)
}

func (c FfiConverterSequenceParsedInclude) LowerExternal(value []ParsedInclude) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ParsedInclude](c, value))
}

func (c FfiConverterSequenceParsedInclude) Write(writer io.Writer, value []ParsedInclude) {
	if len(value) > math.MaxInt32 {
		panic("[]ParsedInclude is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterParsedIncludeINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceParsedInclude struct{}

func (FfiDestroyerSequenceParsedInclude) Destroy(sequence []ParsedInclude) {
	for _, value := range sequence {
		FfiDestroyerParsedInclude{}.Destroy(value)
	}
}

// [`APPVIEW_SERVICE_DID`] for the Go bridge — uniffi cannot export a bare
// `const`, and a hard-coded Go copy could drift, at which point a headerless
// read dials one service while D8 authorized against another.
func AppviewServiceDid() string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_appview_service_did(_uniffiStatus),
		}
	}))
}

// Authorize one request. Pure; see the module docs for the contract.
//
// The Go atproto.pds bridge calls this from its `xrpc.AuthzHook` seat: it
// assembles the input from the route + the verified caller and enforces the
// verdict verbatim. **Go never interprets** — an unrecognized input denies
// here, never falls through to allow there.
//
// Public routes never reach this: `Auth: Public` in the route table *is* their
// authorization, and a request with no authenticated plane has nothing for
// this matrix to decide. `com.atproto.server.getServiceAuth` reaches it
// *twice* — once for the route, once for the requested `lxm`/`aud`.
//
// The export lives here rather than in `fauna-ffi` because the input/verdict
// types do: uniffi-bindgen-go emits one Go package per uniffi namespace and
// cannot resolve a type reference across two, so a function and the types it
// takes must share a crate (the `fauna_mail::verify_inbound` precedent).
//
// Takes the input by value because the FFI boundary hands over an owned
// record; the internal helpers borrow it.
func Authorize(input AuthzInput) AuthzVerdict {
	return FfiConverterAuthzVerdictINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_authorize(FfiConverterAuthzInputINSTANCE.Lower(input), _uniffiStatus),
		}
	}))
}

// A human-readable, one-line rendering of a scope — the string the consent
// surfaces show a person deciding whether to approve.
//
// One owner for BOTH consent surfaces — the browser page `/oauth/authorize`
// serves (F4 slice 6b) and the in-app approval card (slice 6c) — because the
// two render the same pending-consent row side by side, and a wording
// divergence between them is exactly the "is this the same request?" doubt
// the binding code exists to remove.
//
// Deliberately **not** localized here: the strings are English, like the rest
// of the wire-adjacent vocabulary this crate owns. If an app surface needs
// localization later, these match arms are the enumeration to key i18n
// entries from — the grammar knowledge stays in this one place either way.
//
// The fallback is the scope string **verbatim** — an honest "no friendlier
// name" rather than a guess. A scope reaching a consent surface has already
// passed [`scope_grants_something`] at PAR, so the fallback arm is for
// grammar this build predates, not for garbage.
func DescribeScope(scope string) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_describe_scope(FfiConverterStringINSTANCE.Lower(scope), _uniffiStatus),
		}
	}))
}

// Authenticate the client behind a request, if this client authenticates.
//
// `assertion_type` and `assertion` are the two form parameters exactly as
// received — empty strings when absent, so "sent nothing" is representable and
// this closed-world check can rule on it rather than the caller guessing.
//
// # Both directions refuse
//
// A **confidential** client that sends no assertion is refused: its document
// says it authenticates, so an unauthenticated request in its name is a
// request from someone else. A **public** client that sends one is *also*
// refused, rather than having it ignored — the same posture as the loopback
// client's unknown query parameters (`atproto-pds-full.md` § F4 detail): a
// caller that sends credentials to an endpoint which silently discards them
// believes it authenticated, and that belief is worth refusing.
func ValidateClientAssertion(assertionType string, assertion string, client ResolvedClient, expect AssertionExpectations) ClientAssertionVerdict {
	return FfiConverterClientAssertionVerdictINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_validate_client_assertion(FfiConverterStringINSTANCE.Lower(assertionType), FfiConverterStringINSTANCE.Lower(assertion), FfiConverterResolvedClientINSTANCE.Lower(client), FfiConverterAssertionExpectationsINSTANCE.Lower(expect), _uniffiStatus),
		}
	}))
}

// Validate a DPoP proof against the request it claims to cover.
//
// Everything except the signature, the nonce's provenance and the `jti`'s
// novelty is decided here; see the module docs for why those three are the
// caller's.
func ValidateDpopProof(compact string, expect DpopExpectations) DpopVerdict {
	return FfiConverterDpopVerdictINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_validate_dpop_proof(FfiConverterStringINSTANCE.Lower(compact), FfiConverterDpopExpectationsINSTANCE.Lower(expect), _uniffiStatus),
		}
	}))
}

// Decide whether an attacker-directed outbound fetch may proceed.
//
// Exported on this crate rather than through a `fauna-ffi` wrapper for the
// same reason [`crate::authz::authorize`] is: uniffi-bindgen-go emits one Go
// package per namespace and cannot resolve a type across two, so a function
// and the types it takes must share a crate.
func CheckFetchTarget(target FetchTarget) FetchTargetVerdict {
	return FfiConverterFetchTargetVerdictINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_check_fetch_target(FfiConverterFetchTargetINSTANCE.Lower(target), _uniffiStatus),
		}
	}))
}

// Complete a confidential client whose document declared a `jwks_uri`, from
// the body fetched at that URI.
//
// # Why the key set is resolved here rather than at assertion time
//
// A confidential client's keys are fetched **eagerly**, as part of resolving
// the client, and the resolved client carries them. The alternative — fetch
// lazily when an assertion arrives, behind its own cache — was rejected for
// two reasons that compound:
//
// * It would put an outbound fetch **inside the authentication decision**, on
// a path whose whole purpose is to establish who is calling. Resolving
// eagerly keeps authentication a pure function over data already in hand, so
// it has no network failure mode, no timing signal and no ordering question.
// * Lazy resolution needs an answer to "an assertion names a `kid` we have not
// seen — do we re-fetch?", and *both* answers are bad: yes makes an
// attacker-chosen `kid` a fetch amplifier, no makes key rotation silently
// unsupported. Eager resolution never asks it.
//
// The cost is that a rotated key takes effect only when the client cache entry
// expires (15 minutes). That is exactly what a key *set* with `kid`s exists to
// absorb: a client rotating correctly publishes the old and new keys together,
// and any sane overlap is far longer than the cache TTL.
//
// Returns the completed client, or a refusal — the same [`ClientResolution`]
// the document parse returns, so the caller has one shape to handle and one
// place that turns a refusal into a cached negative.
func AttachClientJwks(client ResolvedClient, body string) ClientResolution {
	return FfiConverterClientResolutionINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_attach_client_jwks(FfiConverterResolvedClientINSTANCE.Lower(client), FfiConverterStringINSTANCE.Lower(body), _uniffiStatus),
		}
	}))
}

// Validate a fetched client-metadata document against the `client_id` it was
// fetched from.
//
// `client_id` must be the URL Go actually GET'd — the same string
// [`plan_client_id`] handed back in [`ClientIdPlan::Fetch`]. The document's
// own `client_id` member is compared against it by **exact string equality**,
// per spec: that equality is what makes the URL a self-authenticating
// identity, because a document served at one URL cannot then claim to be a
// client living at another.
func ParseClientMetadata(clientId string, body string) ClientResolution {
	return FfiConverterClientResolutionINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_parse_client_metadata(FfiConverterStringINSTANCE.Lower(clientId), FfiConverterStringINSTANCE.Lower(body), _uniffiStatus),
		}
	}))
}

// Classify a `client_id` into the work Go must do for it.
//
// Pure and network-free. See the module docs for why this classifies rather
// than parses.
func PlanClientId(clientId string) ClientIdPlan {
	return FfiConverterClientIdPlanINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_plan_client_id(FfiConverterStringINSTANCE.Lower(clientId), _uniffiStatus),
		}
	}))
}

// [`pds_host`], exported for the PDS bridge.
func AtprotoPdsHost(apexDomain string) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_atproto_pds_host(FfiConverterStringINSTANCE.Lower(apexDomain), _uniffiStatus),
		}
	}))
}

// [`pds_service_did`], exported for the PDS bridge: the `aud` its resource
// server requires of every access token, and the one the nest mints with.
func AtprotoPdsServiceDid(apexDomain string) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_atproto_pds_service_did(FfiConverterStringINSTANCE.Lower(apexDomain), _uniffiStatus),
		}
	}))
}

// The issuer identifier for a host — its origin, scheme included, with no path
// and no trailing slash. The nest's issuer is this over its apex domain.
//
// This exact string is compared for equality by clients — it is the `iss` of
// every access token, the `issuer` of the AS document, the entry in the
// protected resource's `authorization_servers`, and (per RFC 8414) the origin
// the `.well-known` URL was fetched from. One builder, so those cannot
// disagree.
func OauthIssuer(host string) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_oauth_issuer(FfiConverterStringINSTANCE.Lower(host), _uniffiStatus),
		}
	}))
}

// Render `/.well-known/oauth-protected-resource` for the PDS of the deployment
// whose apex domain is `apex_domain`.
//
// Its whole job is the redirection in § F4 detail: *this resource server's
// authorization server is that one* — the nest's issuer, on a different host
// from the PDS it describes, so a client has to follow it to find the AS.
//
// `bearer_methods_supported` is **`DPoP` only**, never `header`: DPoP is
// mandatory for all client types in ATProto (§ Ecosystem reality item 3), so
// advertising bearer would invite a presentation this server refuses.
func OauthProtectedResourceDocument(apexDomain string) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_oauth_protected_resource_document(FfiConverterStringINSTANCE.Lower(apexDomain), _uniffiStatus),
		}
	}))
}

// Decide a request whose permission sets have been fetched.
//
// `records[i]` must be the **verified** `com.atproto.lexicon.schema` record for
// `pending.includes[i]`, dag-cbor, verbatim as the proof established it.
//
// # Why this takes bytes rather than expansions
//
// Expansion is this module's (`atproto-pds-full.md:333`), so the caller has no
// business performing it and then handing back the result: that would make Go
// the owner of an intermediate representation and give the ignore rules a
// second, silent implementation at the seam. Handing back exactly the bytes
// that were fetched keeps "what was verified" and "what was expanded" the same
// object across the language boundary, and it means a misaligned answer is
// caught by [`expand_permission_set`]'s own `id` check — a record served under
// one NSID that declares another refuses, rather than attaching one set's
// members to another set's name on the consent card.
//
// A set that could not be *resolved* never reaches here: that failure fails the
// whole request at the caller (`atproto-pds-full.md:331`), because the reason
// describes our outbound network and must not be echoed to whoever chose the
// NSID.
func FinishParRequest(pending PendingPar, records [][]byte) ParVerdict {
	return FfiConverterParVerdictINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_finish_par_request(FfiConverterPendingParINSTANCE.Lower(pending), FfiConverterSequenceBytesINSTANCE.Lower(records), _uniffiStatus),
		}
	}))
}

// Validate a pushed authorization request against the client it names, up to
// the point where a permission set would have to be fetched.
//
// `client` must be the [`ResolvedClient`] produced from *this request's*
// `client_id` — the caller resolves first, then validates, and this function
// re-asserts the pairing rather than trusting it.
//
// # Why this returns a plan rather than a verdict
//
// Every refusal a pure function can reach happens **here**, before the caller
// is told to fetch anything: a request with a bad PKCE challenge, an undeclared
// scope, a malformed `include:`, or nine permission sets costs zero DNS
// queries and zero HTTPS requests (`atproto-pds-full.md:331`, the fan-out cap;
// `:332`, NSID syntax validated before any I/O). Only a request that is
// otherwise acceptable is worth resolving a third party's document for.
func PlanParRequest(request ParRequest, client ResolvedClient) ParPlan {
	return FfiConverterParPlanINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_plan_par_request(FfiConverterParRequestINSTANCE.Lower(request), FfiConverterResolvedClientINSTANCE.Lower(client), _uniffiStatus),
		}
	}))
}

// Check one authorization request's include count and total expansion against
// the caps.
//
// One face rather than three constants for PS-b to re-implement at PAR: the
// caps and the sentence explaining a refusal belong to the same owner as the
// grammar they bound. `expanded` is every scope the grant would carry —
// expansion members and directly-requested scopes alike, since the token
// carries one list and the byte budget does not care which is which.
func CheckGrantExpansion(includeCount uint32, expanded []string) GrantExpansion {
	return FfiConverterGrantExpansionINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_check_grant_expansion(FfiConverterUint32INSTANCE.Lower(includeCount), FfiConverterSequenceStringINSTANCE.Lower(expanded), _uniffiStatus),
		}
	}))
}

// Expand a resolved permission set into ordinary granular scopes.
//
// `record_dag_cbor` is the **verified** `com.atproto.lexicon.schema` record —
// see the module docs on why the bytes cross verbatim.
//
// # The document `id` must match the NSID that was asked for
//
// A permission set lives on whatever PDS its authority's DID designates,
// which is usually a large multi-tenant host — the party a prior ruling says to
// verify rather than trust. The MST proof and commit signature prove *that
// host published this record*; they do not prove it is the record for the NSID
// the client named. Checking `id` is what closes the substitution: a host that
// serves `com.evil.wide` under the rkey `com.example.narrow` gets a failed
// resolution, not a silently wider grant.
func ExpandPermissionSet(include ParsedInclude, recordDagCbor []byte) Expansion {
	return FfiConverterExpansionINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_expand_permission_set(FfiConverterParsedIncludeINSTANCE.Lower(include), FfiConverterBytesINSTANCE.Lower(recordDagCbor), _uniffiStatus),
		}
	}))
}

// Parse an `include:<NSID>[?aud=<audience>]` scope.
//
// **Every refusal here happens before any I/O**, which is the point: Go
// fetches only what this parser emitted, so no caller-controlled string
// reaches DNS or an HTTPS request without having been through
// [`nsid_syntax_error`] first.
//
// Unknown query parameters **refuse** rather than being ignored — the
// loopback-client rule (`oauth_client::synthesize_loopback_client`), for the
// same reason: an unrecognized parameter means the client believes it
// configured something this server did not read, and a permission scope is
// the last place to let that pass quietly.
func ParseIncludeScope(scope string) IncludeScope {
	return FfiConverterIncludeScopeINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_bridge_atproto_fn_func_parse_include_scope(FfiConverterStringINSTANCE.Lower(scope), _uniffiStatus),
		}
	}))
}
