package fauna_launch_machine

// #include <fauna_launch_machine.h>
import "C"

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"io"
	"math"
	"reflect"
	"runtime"
	"runtime/cgo"
	"sync"
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
		C.ffi_fauna_launch_machine_rustbuffer_free(cb.inner, status)
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
		return C.ffi_fauna_launch_machine_rustbuffer_from_bytes(foreign, status)
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

	FfiConverterLaunchObserverINSTANCE.register()
	FfiConverterLaunchPersistenceINSTANCE.register()
	FfiConverterPendingProvisionStoreINSTANCE.register()
	uniffiCheckChecksums()
}

func uniffiCheckChecksums() {
	// Get the bindings contract version from our ComponentInterface
	bindingsContractVersion := 30
	// Get the scaffolding contract version by calling the into the dylib
	scaffoldingContractVersion := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint32_t {
		return C.ffi_fauna_launch_machine_uniffi_contract_version()
	})
	if bindingsContractVersion != int(scaffoldingContractVersion) {
		// If this happens try cleaning and rebuilding your project
		panic("fauna_launch_machine: UniFFI contract version mismatch")
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_func_resolved_dial_url()
		})
		if checksum != 53876 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_func_resolved_dial_url: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_func_mint_and_persist_pending_factory_reset()
		})
		if checksum != 19072 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_func_mint_and_persist_pending_factory_reset: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchmachine_current_bearer()
		})
		if checksum != 45461 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchmachine_current_bearer: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchmachine_identity_changed_authority()
		})
		if checksum != 57894 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchmachine_identity_changed_authority: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchmachine_notify_401()
		})
		if checksum != 14103 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchmachine_notify_401: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchmachine_refresh_token()
		})
		if checksum != 61311 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchmachine_refresh_token: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchmachine_retry_silent_challenge()
		})
		if checksum != 64577 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchmachine_retry_silent_challenge: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchmachine_snapshot()
		})
		if checksum != 46039 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchmachine_snapshot: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchmachine_start()
		})
		if checksum != 4367 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchmachine_start: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchmachine_trust_nest_identity()
		})
		if checksum != 32815 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchmachine_trust_nest_identity: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchobserver_on_changed()
		})
		if checksum != 25527 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchobserver_on_changed: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchpersistence_account_index_refusal()
		})
		if checksum != 10627 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchpersistence_account_index_refusal: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchpersistence_load_identity()
		})
		if checksum != 50610 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchpersistence_load_identity: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchpersistence_load_nest_url()
		})
		if checksum != 19487 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchpersistence_load_nest_url: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchpersistence_load_pending_invite()
		})
		if checksum != 20826 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchpersistence_load_pending_invite: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchpersistence_load_awaiting_dns()
		})
		if checksum != 11725 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchpersistence_load_awaiting_dns: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchpersistence_load_pending_factory_reset()
		})
		if checksum != 52297 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchpersistence_load_pending_factory_reset: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchpersistence_save_pending_factory_reset()
		})
		if checksum != 2467 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchpersistence_save_pending_factory_reset: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchpersistence_delete_pending_factory_reset()
		})
		if checksum != 42040 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchpersistence_delete_pending_factory_reset: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchpersistence_save_authenticated()
		})
		if checksum != 43268 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchpersistence_save_authenticated: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchpersistence_delete_pending_invite()
		})
		if checksum != 27689 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchpersistence_delete_pending_invite: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchpersistence_load_reach_ipv4()
		})
		if checksum != 53864 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchpersistence_load_reach_ipv4: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_launchpersistence_delete_reach_ipv4()
		})
		if checksum != 65326 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_launchpersistence_delete_reach_ipv4: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_pendingprovisionstore_save_awaiting_dns()
		})
		if checksum != 3289 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_pendingprovisionstore_save_awaiting_dns: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_method_pendingprovisionstore_clear_awaiting_dns()
		})
		if checksum != 46549 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_method_pendingprovisionstore_clear_awaiting_dns: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_launch_machine_checksum_constructor_launchmachine_new()
		})
		if checksum != 14227 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_launch_machine: uniffi_fauna_launch_machine_checksum_constructor_launchmachine_new: UniFFI API checksum mismatch")
		}
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

type LaunchMachineInterface interface {
	// Bearer token if the machine is currently `Online`. HTTP layers
	// in clients call this on every request and fall back to nothing
	// (queueing or 401-driven refresh) otherwise.
	CurrentBearer() *string
	// The nest authority this machine met a changed identity at, when it is
	// parked in `IdentityChanged` — `None` in every other state.
	//
	// A narrow read rather than a new `LaunchPhase` field on purpose:
	// `LaunchPhase` is UniFFI-exported and switched over exhaustively by all
	// seven apps (`State::Superseded`'s docs carry the full reasoning), and the
	// only caller that needs the host is `fauna_nest_http::LaunchMachineBearer`,
	// naming the nest in the `ApiError::NestIdentityChanged` it now raises
	// instead of a generic transport failure.
	//
	// Native-only, like its one caller: `authority_of` lives in the native
	// `fauna-anon-client` (the wasm trust seam is `fauna-rpc-wasm`'s instead).
	IdentityChangedAuthority() *string
	// HTTP layer reports a 401 on a content endpoint. Trigger an
	// immediate refresh over the silent challenge so the caller can
	// retry. Headline new behavior — the per-app HTTP layers today
	// don't have a 401-reactive interceptor. No-op if not in a
	// refreshable state.
	Notify401()
	// Explicit token refresh. Caller-driven (e.g., before launching a
	// long-running operation). Mints over the silent challenge —
	// `fauna.auth.challenge` + `fauna.auth.verify`, the launch ceremony —
	// so a wrong client clock cannot refuse it (`login.md` § When to use
	// which). No-op if the machine isn't in a refreshable state.
	RefreshToken()
	// Re-run the silent-challenge fast path. Meaningful only from
	// `Offline { transient: true }` — that's the surface a user sees
	// when the first launch attempt hit a transient network/server
	// error. Other phases (Online, Refreshing, WizardAt, the terminal
	// `Offline { transient: false }`, the in-flight SilentChallenge,
	// or the early Boot/Hydrating) don't make sense to retry: either
	// the user is already authenticated, the wizard owns the next
	// step, or the failure is terminal and needs explicit intervention.
	// All of those cases return without side effects.
	//
	// Re-reads identity + nest_url from persistence; if either is
	// missing, also a no-op.
	RetrySilentChallenge()
	// Read-only view for clients to render UI. Cheap.
	Snapshot() LaunchSnapshot
	// Drive the launch flow. Reads from the persistence trait, branches
	// on the four cases per `docs/goal/behavior/onboarding.md` § App-launch
	// routing, and transitions to the appropriate phase. Case 1 (identity +
	// nest_url) runs the silent challenge inline over the [`AuthConnector`].
	Start()
	// The explicit "trust this nest" recovery from
	// [`LaunchPhase::IdentityChanged`]: forget the TOFU pin for the nest (via
	// the connector's trust seam — the `ssh-keygen -R host` analogue), then
	// re-run the silent challenge, which re-TOFUs against whatever identity
	// the nest now proves. Meaningful only from `IdentityChanged`; every
	// other phase returns without side effects — in particular the pin is
	// NEVER forgotten outside this user-approved action (security.md
	// § Transport trust: no silent re-pin, ever).
	TrustNestIdentity()
}
type LaunchMachine struct {
	ffiObject FfiObject
}

func NewLaunchMachine(observer LaunchObserver, persistence LaunchPersistence) *LaunchMachine {
	return FfiConverterLaunchMachineINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_fauna_launch_machine_fn_constructor_launchmachine_new(FfiConverterLaunchObserverINSTANCE.Lower(observer), FfiConverterLaunchPersistenceINSTANCE.Lower(persistence), _uniffiStatus)
	}))
}

// Bearer token if the machine is currently `Online`. HTTP layers
// in clients call this on every request and fall back to nothing
// (queueing or 401-driven refresh) otherwise.
func (_self *LaunchMachine) CurrentBearer() *string {
	_pointer := _self.ffiObject.incrementPointer("*LaunchMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_launch_machine_fn_method_launchmachine_current_bearer(
				_pointer, _uniffiStatus),
		}
	}))
}

// The nest authority this machine met a changed identity at, when it is
// parked in `IdentityChanged` — `None` in every other state.
//
// A narrow read rather than a new `LaunchPhase` field on purpose:
// `LaunchPhase` is UniFFI-exported and switched over exhaustively by all
// seven apps (`State::Superseded`'s docs carry the full reasoning), and the
// only caller that needs the host is `fauna_nest_http::LaunchMachineBearer`,
// naming the nest in the `ApiError::NestIdentityChanged` it now raises
// instead of a generic transport failure.
//
// Native-only, like its one caller: `authority_of` lives in the native
// `fauna-anon-client` (the wasm trust seam is `fauna-rpc-wasm`'s instead).
func (_self *LaunchMachine) IdentityChangedAuthority() *string {
	_pointer := _self.ffiObject.incrementPointer("*LaunchMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_launch_machine_fn_method_launchmachine_identity_changed_authority(
				_pointer, _uniffiStatus),
		}
	}))
}

// HTTP layer reports a 401 on a content endpoint. Trigger an
// immediate refresh over the silent challenge so the caller can
// retry. Headline new behavior — the per-app HTTP layers today
// don't have a 401-reactive interceptor. No-op if not in a
// refreshable state.
func (_self *LaunchMachine) Notify401() {
	_pointer := _self.ffiObject.incrementPointer("*LaunchMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_launch_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_launch_machine_fn_method_launchmachine_notify_401(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_launch_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_launch_machine_rust_future_free_void(handle)
		},
	)

}

// Explicit token refresh. Caller-driven (e.g., before launching a
// long-running operation). Mints over the silent challenge —
// `fauna.auth.challenge` + `fauna.auth.verify`, the launch ceremony —
// so a wrong client clock cannot refuse it (`login.md` § When to use
// which). No-op if the machine isn't in a refreshable state.
func (_self *LaunchMachine) RefreshToken() {
	_pointer := _self.ffiObject.incrementPointer("*LaunchMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_launch_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_launch_machine_fn_method_launchmachine_refresh_token(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_launch_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_launch_machine_rust_future_free_void(handle)
		},
	)

}

// Re-run the silent-challenge fast path. Meaningful only from
// `Offline { transient: true }` — that's the surface a user sees
// when the first launch attempt hit a transient network/server
// error. Other phases (Online, Refreshing, WizardAt, the terminal
// `Offline { transient: false }`, the in-flight SilentChallenge,
// or the early Boot/Hydrating) don't make sense to retry: either
// the user is already authenticated, the wizard owns the next
// step, or the failure is terminal and needs explicit intervention.
// All of those cases return without side effects.
//
// Re-reads identity + nest_url from persistence; if either is
// missing, also a no-op.
func (_self *LaunchMachine) RetrySilentChallenge() {
	_pointer := _self.ffiObject.incrementPointer("*LaunchMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_launch_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_launch_machine_fn_method_launchmachine_retry_silent_challenge(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_launch_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_launch_machine_rust_future_free_void(handle)
		},
	)

}

// Read-only view for clients to render UI. Cheap.
func (_self *LaunchMachine) Snapshot() LaunchSnapshot {
	_pointer := _self.ffiObject.incrementPointer("*LaunchMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterLaunchSnapshotINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_launch_machine_fn_method_launchmachine_snapshot(
				_pointer, _uniffiStatus),
		}
	}))
}

// Drive the launch flow. Reads from the persistence trait, branches
// on the four cases per `docs/goal/behavior/onboarding.md` § App-launch
// routing, and transitions to the appropriate phase. Case 1 (identity +
// nest_url) runs the silent challenge inline over the [`AuthConnector`].
func (_self *LaunchMachine) Start() {
	_pointer := _self.ffiObject.incrementPointer("*LaunchMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_launch_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_launch_machine_fn_method_launchmachine_start(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_launch_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_launch_machine_rust_future_free_void(handle)
		},
	)

}

// The explicit "trust this nest" recovery from
// [`LaunchPhase::IdentityChanged`]: forget the TOFU pin for the nest (via
// the connector's trust seam — the `ssh-keygen -R host` analogue), then
// re-run the silent challenge, which re-TOFUs against whatever identity
// the nest now proves. Meaningful only from `IdentityChanged`; every
// other phase returns without side effects — in particular the pin is
// NEVER forgotten outside this user-approved action (security.md
// § Transport trust: no silent re-pin, ever).
func (_self *LaunchMachine) TrustNestIdentity() {
	_pointer := _self.ffiObject.incrementPointer("*LaunchMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_launch_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_launch_machine_fn_method_launchmachine_trust_nest_identity(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_launch_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_launch_machine_rust_future_free_void(handle)
		},
	)

}
func (object *LaunchMachine) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterLaunchMachine struct{}

var FfiConverterLaunchMachineINSTANCE = FfiConverterLaunchMachine{}

func (c FfiConverterLaunchMachine) Lift(handle C.uint64_t) *LaunchMachine {
	result := &LaunchMachine{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_fauna_launch_machine_fn_clone_launchmachine(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_fauna_launch_machine_fn_free_launchmachine(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*LaunchMachine).Destroy)
	return result
}

func (c FfiConverterLaunchMachine) Read(reader io.Reader) *LaunchMachine {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterLaunchMachine) Lower(value *LaunchMachine) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*LaunchMachine")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterLaunchMachine) Write(writer io.Writer, value *LaunchMachine) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalLaunchMachine(handle uint64) *LaunchMachine {
	return FfiConverterLaunchMachineINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalLaunchMachine(value *LaunchMachine) uint64 {
	return uint64(FfiConverterLaunchMachineINSTANCE.Lower(value))
}

type FfiDestroyerLaunchMachine struct{}

func (_ FfiDestroyerLaunchMachine) Destroy(value *LaunchMachine) {
	value.Destroy()
}

type LaunchObserver interface {
	// Called whenever the machine's observable state changes. The
	// observer reads a fresh snapshot via the machine's own getter.
	OnChanged()
}
type LaunchObserverImpl struct {
	ffiObject FfiObject
}

// Called whenever the machine's observable state changes. The
// observer reads a fresh snapshot via the machine's own getter.
func (_self *LaunchObserverImpl) OnChanged() {
	_pointer := _self.ffiObject.incrementPointer("LaunchObserver")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_launch_machine_fn_method_launchobserver_on_changed(
			_pointer, _uniffiStatus)
		return false
	})
}
func (object *LaunchObserverImpl) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterLaunchObserver struct {
	handleMap *concurrentHandleMap[LaunchObserver]
}

var FfiConverterLaunchObserverINSTANCE = FfiConverterLaunchObserver{
	handleMap: newConcurrentHandleMap[LaunchObserver](),
}

func (c FfiConverterLaunchObserver) Lift(handle C.uint64_t) LaunchObserver {
	if uint64(handle)&1 == 0 {
		// Rust-generated handle (even), construct a new object wrapping the handle
		result := &LaunchObserverImpl{
			newFfiObject(
				handle,
				func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
					return C.uniffi_fauna_launch_machine_fn_clone_launchobserver(handle, status)
				},
				func(handle C.uint64_t, status *C.RustCallStatus) {
					C.uniffi_fauna_launch_machine_fn_free_launchobserver(handle, status)
				},
			),
		}
		runtime.SetFinalizer(result, (*LaunchObserverImpl).Destroy)
		return result
	} else {
		// Go-generated handle (odd), retrieve from the handle map
		val, ok := c.handleMap.tryGet(uint64(handle))
		if !ok {
			panic(fmt.Errorf("no callback in handle map: %d", handle))
		}
		c.handleMap.remove(uint64(handle))
		return val
	}
}

func (c FfiConverterLaunchObserver) Read(reader io.Reader) LaunchObserver {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterLaunchObserver) Lower(value LaunchObserver) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	if val, ok := value.(*LaunchObserverImpl); ok {
		// Rust-backed object, clone the handle
		handle := val.ffiObject.incrementPointer("LaunchObserver")
		defer val.ffiObject.decrementPointer()
		return handle
	} else {
		// Go-backed object, insert into handle map
		return C.uint64_t(c.handleMap.insert(value))
	}
}

func (c FfiConverterLaunchObserver) Write(writer io.Writer, value LaunchObserver) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalLaunchObserver(handle uint64) LaunchObserver {
	return FfiConverterLaunchObserverINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalLaunchObserver(value LaunchObserver) uint64 {
	return uint64(FfiConverterLaunchObserverINSTANCE.Lower(value))
}

type FfiDestroyerLaunchObserver struct{}

func (_ FfiDestroyerLaunchObserver) Destroy(value LaunchObserver) {
	if val, ok := value.(*LaunchObserverImpl); ok {
		val.Destroy()
	}
}

type uniffiCallbackResult C.int8_t

const (
	uniffiIdxCallbackFree               uniffiCallbackResult = 0
	uniffiCallbackResultSuccess         uniffiCallbackResult = 0
	uniffiCallbackResultError           uniffiCallbackResult = 1
	uniffiCallbackUnexpectedResultError uniffiCallbackResult = 2
	uniffiCallbackCancelled             uniffiCallbackResult = 3
)

type concurrentHandleMap[T any] struct {
	handles       map[uint64]T
	currentHandle uint64
	lock          sync.RWMutex
}

func newConcurrentHandleMap[T any]() *concurrentHandleMap[T] {
	return &concurrentHandleMap[T]{
		handles:       map[uint64]T{},
		currentHandle: 1,
	}
}

func (cm *concurrentHandleMap[T]) insert(obj T) uint64 {
	cm.lock.Lock()
	defer cm.lock.Unlock()

	handle := cm.currentHandle
	cm.currentHandle = cm.currentHandle + 2
	cm.handles[handle] = obj
	return handle
}

func (cm *concurrentHandleMap[T]) remove(handle uint64) {
	cm.lock.Lock()
	defer cm.lock.Unlock()

	delete(cm.handles, handle)
}

func (cm *concurrentHandleMap[T]) tryGet(handle uint64) (T, bool) {
	cm.lock.RLock()
	defer cm.lock.RUnlock()

	val, ok := cm.handles[handle]
	return val, ok
}

//export fauna_launch_machine_observer_cgo_dispatchCallbackInterfaceLaunchObserverMethod0
func fauna_launch_machine_observer_cgo_dispatchCallbackInterfaceLaunchObserverMethod0(uniffiHandle C.uint64_t, uniffiOutReturn *C.void, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterLaunchObserverINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	uniffiObj.OnChanged()

}

var UniffiVTableCallbackInterfaceLaunchObserverINSTANCE = C.UniffiVTableCallbackInterfaceLaunchObserver{
	uniffiFree:  (C.UniffiCallbackInterfaceFree)(C.fauna_launch_machine_observer_cgo_dispatchCallbackInterfaceLaunchObserverFree),
	uniffiClone: (C.UniffiCallbackInterfaceClone)(C.fauna_launch_machine_observer_cgo_dispatchCallbackInterfaceLaunchObserverClone),
	onChanged:   (C.UniffiCallbackInterfaceLaunchObserverMethod0)(C.fauna_launch_machine_observer_cgo_dispatchCallbackInterfaceLaunchObserverMethod0),
}

//export fauna_launch_machine_observer_cgo_dispatchCallbackInterfaceLaunchObserverFree
func fauna_launch_machine_observer_cgo_dispatchCallbackInterfaceLaunchObserverFree(handle C.uint64_t) {
	FfiConverterLaunchObserverINSTANCE.handleMap.remove(uint64(handle))
}

//export fauna_launch_machine_observer_cgo_dispatchCallbackInterfaceLaunchObserverClone
func fauna_launch_machine_observer_cgo_dispatchCallbackInterfaceLaunchObserverClone(handle C.uint64_t) C.uint64_t {
	val, ok := FfiConverterLaunchObserverINSTANCE.handleMap.tryGet(uint64(handle))
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}
	return C.uint64_t(FfiConverterLaunchObserverINSTANCE.handleMap.insert(val))
}

func (c FfiConverterLaunchObserver) register() {
	C.uniffi_fauna_launch_machine_fn_init_callback_vtable_launchobserver(&UniffiVTableCallbackInterfaceLaunchObserverINSTANCE)
}

// Long-term store interface.
type LaunchPersistence interface {
	// The account index is present but this build cannot use it
	// (`onboarding.md` § App-launch routing — the "present but unreadable"
	// row). Read **before every other row**, for the same reason the
	// pending-factory-reset row is: on either verdict the registry answers no
	// session account, so every row below this one reads the install as having
	// **no identity** and routes the user into fresh onboarding — with their
	// accounts sitting intact behind a blob this build merely cannot parse.
	//
	// No default: `uniffi::export`'d trait methods cannot have one. Every
	// implementor is Rust-side today (no app implements this trait — each
	// receives the shared `RegistryLaunchPersistence` from
	// `FfiAccountRegistry::launch_persistence`), so an implementation with no
	// account index answers `None` in one line.
	AccountIndexRefusal() *AccountIndexRefusal
	// 32-byte Ed25519 secret if an identity has been imported or generated.
	LoadIdentity() *[]byte
	// Cached nest URL from a prior successful authentication.
	LoadNestUrl() *string
	// Pending-invite slot. At most one outstanding invite per identity.
	LoadPendingInvite() *PendingInviteRecord
	// Awaiting-manual-dns slot. At most one deferred-DNS nest per identity.
	//
	// Read **before** `load_nest_url`'s silent-challenge row (see
	// [`super::LaunchMachine::start`]): while DNS is still pending the nest is
	// unreachable by definition, so a silent challenge against a saved
	// `nest_url` would only fail through to the `launch_retry` surface.
	//
	// The client clears the slot at the wizard's `LoggedIn` terminal — inside
	// the shared `persist_logged_in` moment, never at the claim itself
	// (`onboarding.md` § Long-term store contract, ratified 2026-09-21) —
	// deletion is a store-side concern (the machine never writes it), so
	// there is no `delete_awaiting_dns` on this trait.
	LoadAwaitingDns() *AwaitingDnsRecord
	// Pending-factory-reset slot. At most one outstanding reset per identity.
	//
	// Read **before** every other row (see [`super::LaunchMachine::start`]): the
	// box this identity last authenticated against has just been wiped to
	// fresh/unclaimed, so a silent challenge against the saved `nest_url` would
	// only fail through to the `launch_retry` surface, and the claim the user
	// must complete is the one this row pins.
	LoadPendingFactoryReset() *PendingFactoryResetRecord
	// Write the pending-factory-reset slot. Called **before** the reset is
	// dispatched, via [`mint_and_persist_pending_factory_reset`] — never
	// directly, so the code cannot be held without having been persisted.
	//
	// Unlike [`Self::save_authenticated`], this write must be **durable before
	// it returns**: the whole point of the row is to survive a SIGKILL that
	// lands microseconds later, so an implementation that defers the write to a
	// background task reopens gap CR-1.
	SavePendingFactoryReset(record PendingFactoryResetRecord)
	// Clear the pending-factory-reset slot once the re-claim completes.
	DeletePendingFactoryReset()
	// Persist `(nest_url, handle, domain, tier)` after a successful silent
	// challenge or wizard completion. Each app caches all four in its
	// long-term store (libsecret on Linux, Keychain on Apple, etc.) so
	// the next relaunch can show "Welcome back, @handle@domain · tier"
	// while the silent challenge is in flight. Implementation may write
	// asynchronously.
	SaveAuthenticated(nestUrl string, userHandle string, domain string, tier string)
	// Clear the pending-invite slot after a successful wizard completion.
	DeletePendingInvite()
	// The account's **reach hint** — the freshly-provisioned box's public IPv4,
	// kept beside `nest_url` while the domain is still propagating
	// (`onboarding.md` § Reach hint, `long-term-store.md` § Multi-account
	// evolution). `None` is the ordinary case and means "dial the domain, once,
	// exactly as before": every account that did not provision its own box —
	// a second device, any sign-in by handle — has no hint and needs none.
	//
	// ⚠ **Required, not defaulted** — this trait is `uniffi::export(with_foreign)`
	// and UniFFI refuses a default body on an exported trait method. A store
	// with no hint to offer therefore writes the `None` out explicitly, which
	// is the honest spelling anyway: "this store never captured one".
	LoadReachIpv4() *string
	// Drop the reach hint. Called on the **first successful domain dial** and
	// at no other time (`onboarding.md` § Reach hint): a hint that merely
	// failed is a failed fallback, not a wrong address, and deleting it there
	// would throw away the one thing that can reach a box whose DNS is still
	// hours out.
	DeleteReachIpv4()
}

// Long-term store interface.
type LaunchPersistenceImpl struct {
	ffiObject FfiObject
}

// The account index is present but this build cannot use it
// (`onboarding.md` § App-launch routing — the "present but unreadable"
// row). Read **before every other row**, for the same reason the
// pending-factory-reset row is: on either verdict the registry answers no
// session account, so every row below this one reads the install as having
// **no identity** and routes the user into fresh onboarding — with their
// accounts sitting intact behind a blob this build merely cannot parse.
//
// No default: `uniffi::export`'d trait methods cannot have one. Every
// implementor is Rust-side today (no app implements this trait — each
// receives the shared `RegistryLaunchPersistence` from
// `FfiAccountRegistry::launch_persistence`), so an implementation with no
// account index answers `None` in one line.
func (_self *LaunchPersistenceImpl) AccountIndexRefusal() *AccountIndexRefusal {
	_pointer := _self.ffiObject.incrementPointer("LaunchPersistence")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalAccountIndexRefusalINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_launch_machine_fn_method_launchpersistence_account_index_refusal(
				_pointer, _uniffiStatus),
		}
	}))
}

// 32-byte Ed25519 secret if an identity has been imported or generated.
func (_self *LaunchPersistenceImpl) LoadIdentity() *[]byte {
	_pointer := _self.ffiObject.incrementPointer("LaunchPersistence")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalBytesINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_launch_machine_fn_method_launchpersistence_load_identity(
				_pointer, _uniffiStatus),
		}
	}))
}

// Cached nest URL from a prior successful authentication.
func (_self *LaunchPersistenceImpl) LoadNestUrl() *string {
	_pointer := _self.ffiObject.incrementPointer("LaunchPersistence")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_launch_machine_fn_method_launchpersistence_load_nest_url(
				_pointer, _uniffiStatus),
		}
	}))
}

// Pending-invite slot. At most one outstanding invite per identity.
func (_self *LaunchPersistenceImpl) LoadPendingInvite() *PendingInviteRecord {
	_pointer := _self.ffiObject.incrementPointer("LaunchPersistence")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalPendingInviteRecordINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_launch_machine_fn_method_launchpersistence_load_pending_invite(
				_pointer, _uniffiStatus),
		}
	}))
}

// Awaiting-manual-dns slot. At most one deferred-DNS nest per identity.
//
// Read **before** `load_nest_url`'s silent-challenge row (see
// [`super::LaunchMachine::start`]): while DNS is still pending the nest is
// unreachable by definition, so a silent challenge against a saved
// `nest_url` would only fail through to the `launch_retry` surface.
//
// The client clears the slot at the wizard's `LoggedIn` terminal — inside
// the shared `persist_logged_in` moment, never at the claim itself
// (`onboarding.md` § Long-term store contract, ratified 2026-09-21) —
// deletion is a store-side concern (the machine never writes it), so
// there is no `delete_awaiting_dns` on this trait.
func (_self *LaunchPersistenceImpl) LoadAwaitingDns() *AwaitingDnsRecord {
	_pointer := _self.ffiObject.incrementPointer("LaunchPersistence")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalAwaitingDnsRecordINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_launch_machine_fn_method_launchpersistence_load_awaiting_dns(
				_pointer, _uniffiStatus),
		}
	}))
}

// Pending-factory-reset slot. At most one outstanding reset per identity.
//
// Read **before** every other row (see [`super::LaunchMachine::start`]): the
// box this identity last authenticated against has just been wiped to
// fresh/unclaimed, so a silent challenge against the saved `nest_url` would
// only fail through to the `launch_retry` surface, and the claim the user
// must complete is the one this row pins.
func (_self *LaunchPersistenceImpl) LoadPendingFactoryReset() *PendingFactoryResetRecord {
	_pointer := _self.ffiObject.incrementPointer("LaunchPersistence")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalPendingFactoryResetRecordINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_launch_machine_fn_method_launchpersistence_load_pending_factory_reset(
				_pointer, _uniffiStatus),
		}
	}))
}

// Write the pending-factory-reset slot. Called **before** the reset is
// dispatched, via [`mint_and_persist_pending_factory_reset`] — never
// directly, so the code cannot be held without having been persisted.
//
// Unlike [`Self::save_authenticated`], this write must be **durable before
// it returns**: the whole point of the row is to survive a SIGKILL that
// lands microseconds later, so an implementation that defers the write to a
// background task reopens gap CR-1.
func (_self *LaunchPersistenceImpl) SavePendingFactoryReset(record PendingFactoryResetRecord) {
	_pointer := _self.ffiObject.incrementPointer("LaunchPersistence")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_launch_machine_fn_method_launchpersistence_save_pending_factory_reset(
			_pointer, FfiConverterPendingFactoryResetRecordINSTANCE.Lower(record), _uniffiStatus)
		return false
	})
}

// Clear the pending-factory-reset slot once the re-claim completes.
func (_self *LaunchPersistenceImpl) DeletePendingFactoryReset() {
	_pointer := _self.ffiObject.incrementPointer("LaunchPersistence")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_launch_machine_fn_method_launchpersistence_delete_pending_factory_reset(
			_pointer, _uniffiStatus)
		return false
	})
}

// Persist `(nest_url, handle, domain, tier)` after a successful silent
// challenge or wizard completion. Each app caches all four in its
// long-term store (libsecret on Linux, Keychain on Apple, etc.) so
// the next relaunch can show "Welcome back, @handle@domain · tier"
// while the silent challenge is in flight. Implementation may write
// asynchronously.
func (_self *LaunchPersistenceImpl) SaveAuthenticated(nestUrl string, userHandle string, domain string, tier string) {
	_pointer := _self.ffiObject.incrementPointer("LaunchPersistence")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_launch_machine_fn_method_launchpersistence_save_authenticated(
			_pointer, FfiConverterStringINSTANCE.Lower(nestUrl), FfiConverterStringINSTANCE.Lower(userHandle), FfiConverterStringINSTANCE.Lower(domain), FfiConverterStringINSTANCE.Lower(tier), _uniffiStatus)
		return false
	})
}

// Clear the pending-invite slot after a successful wizard completion.
func (_self *LaunchPersistenceImpl) DeletePendingInvite() {
	_pointer := _self.ffiObject.incrementPointer("LaunchPersistence")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_launch_machine_fn_method_launchpersistence_delete_pending_invite(
			_pointer, _uniffiStatus)
		return false
	})
}

// The account's **reach hint** — the freshly-provisioned box's public IPv4,
// kept beside `nest_url` while the domain is still propagating
// (`onboarding.md` § Reach hint, `long-term-store.md` § Multi-account
// evolution). `None` is the ordinary case and means "dial the domain, once,
// exactly as before": every account that did not provision its own box —
// a second device, any sign-in by handle — has no hint and needs none.
//
// ⚠ **Required, not defaulted** — this trait is `uniffi::export(with_foreign)`
// and UniFFI refuses a default body on an exported trait method. A store
// with no hint to offer therefore writes the `None` out explicitly, which
// is the honest spelling anyway: "this store never captured one".
func (_self *LaunchPersistenceImpl) LoadReachIpv4() *string {
	_pointer := _self.ffiObject.incrementPointer("LaunchPersistence")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_launch_machine_fn_method_launchpersistence_load_reach_ipv4(
				_pointer, _uniffiStatus),
		}
	}))
}

// Drop the reach hint. Called on the **first successful domain dial** and
// at no other time (`onboarding.md` § Reach hint): a hint that merely
// failed is a failed fallback, not a wrong address, and deleting it there
// would throw away the one thing that can reach a box whose DNS is still
// hours out.
func (_self *LaunchPersistenceImpl) DeleteReachIpv4() {
	_pointer := _self.ffiObject.incrementPointer("LaunchPersistence")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_launch_machine_fn_method_launchpersistence_delete_reach_ipv4(
			_pointer, _uniffiStatus)
		return false
	})
}
func (object *LaunchPersistenceImpl) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterLaunchPersistence struct {
	handleMap *concurrentHandleMap[LaunchPersistence]
}

var FfiConverterLaunchPersistenceINSTANCE = FfiConverterLaunchPersistence{
	handleMap: newConcurrentHandleMap[LaunchPersistence](),
}

func (c FfiConverterLaunchPersistence) Lift(handle C.uint64_t) LaunchPersistence {
	if uint64(handle)&1 == 0 {
		// Rust-generated handle (even), construct a new object wrapping the handle
		result := &LaunchPersistenceImpl{
			newFfiObject(
				handle,
				func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
					return C.uniffi_fauna_launch_machine_fn_clone_launchpersistence(handle, status)
				},
				func(handle C.uint64_t, status *C.RustCallStatus) {
					C.uniffi_fauna_launch_machine_fn_free_launchpersistence(handle, status)
				},
			),
		}
		runtime.SetFinalizer(result, (*LaunchPersistenceImpl).Destroy)
		return result
	} else {
		// Go-generated handle (odd), retrieve from the handle map
		val, ok := c.handleMap.tryGet(uint64(handle))
		if !ok {
			panic(fmt.Errorf("no callback in handle map: %d", handle))
		}
		c.handleMap.remove(uint64(handle))
		return val
	}
}

func (c FfiConverterLaunchPersistence) Read(reader io.Reader) LaunchPersistence {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterLaunchPersistence) Lower(value LaunchPersistence) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	if val, ok := value.(*LaunchPersistenceImpl); ok {
		// Rust-backed object, clone the handle
		handle := val.ffiObject.incrementPointer("LaunchPersistence")
		defer val.ffiObject.decrementPointer()
		return handle
	} else {
		// Go-backed object, insert into handle map
		return C.uint64_t(c.handleMap.insert(value))
	}
}

func (c FfiConverterLaunchPersistence) Write(writer io.Writer, value LaunchPersistence) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalLaunchPersistence(handle uint64) LaunchPersistence {
	return FfiConverterLaunchPersistenceINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalLaunchPersistence(value LaunchPersistence) uint64 {
	return uint64(FfiConverterLaunchPersistenceINSTANCE.Lower(value))
}

type FfiDestroyerLaunchPersistence struct{}

func (_ FfiDestroyerLaunchPersistence) Destroy(value LaunchPersistence) {
	if val, ok := value.(*LaunchPersistenceImpl); ok {
		val.Destroy()
	}
}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod0
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod0(uniffiHandle C.uint64_t, uniffiOutReturn *C.RustBuffer, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterLaunchPersistenceINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	res :=
		uniffiObj.AccountIndexRefusal()

	*uniffiOutReturn = FfiConverterOptionalAccountIndexRefusalINSTANCE.Lower(res)
}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod1
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod1(uniffiHandle C.uint64_t, uniffiOutReturn *C.RustBuffer, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterLaunchPersistenceINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	res :=
		uniffiObj.LoadIdentity()

	*uniffiOutReturn = FfiConverterOptionalBytesINSTANCE.Lower(res)
}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod2
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod2(uniffiHandle C.uint64_t, uniffiOutReturn *C.RustBuffer, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterLaunchPersistenceINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	res :=
		uniffiObj.LoadNestUrl()

	*uniffiOutReturn = FfiConverterOptionalStringINSTANCE.Lower(res)
}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod3
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod3(uniffiHandle C.uint64_t, uniffiOutReturn *C.RustBuffer, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterLaunchPersistenceINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	res :=
		uniffiObj.LoadPendingInvite()

	*uniffiOutReturn = FfiConverterOptionalPendingInviteRecordINSTANCE.Lower(res)
}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod4
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod4(uniffiHandle C.uint64_t, uniffiOutReturn *C.RustBuffer, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterLaunchPersistenceINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	res :=
		uniffiObj.LoadAwaitingDns()

	*uniffiOutReturn = FfiConverterOptionalAwaitingDnsRecordINSTANCE.Lower(res)
}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod5
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod5(uniffiHandle C.uint64_t, uniffiOutReturn *C.RustBuffer, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterLaunchPersistenceINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	res :=
		uniffiObj.LoadPendingFactoryReset()

	*uniffiOutReturn = FfiConverterOptionalPendingFactoryResetRecordINSTANCE.Lower(res)
}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod6
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod6(uniffiHandle C.uint64_t, record C.RustBuffer, uniffiOutReturn *C.void, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterLaunchPersistenceINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	uniffiObj.SavePendingFactoryReset(
		FfiConverterPendingFactoryResetRecordINSTANCE.Lift(GoRustBuffer{
			inner: record,
		}),
	)

}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod7
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod7(uniffiHandle C.uint64_t, uniffiOutReturn *C.void, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterLaunchPersistenceINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	uniffiObj.DeletePendingFactoryReset()

}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod8
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod8(uniffiHandle C.uint64_t, nestUrl C.RustBuffer, userHandle C.RustBuffer, domain C.RustBuffer, tier C.RustBuffer, uniffiOutReturn *C.void, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterLaunchPersistenceINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	uniffiObj.SaveAuthenticated(
		FfiConverterStringINSTANCE.Lift(GoRustBuffer{
			inner: nestUrl,
		}),
		FfiConverterStringINSTANCE.Lift(GoRustBuffer{
			inner: userHandle,
		}),
		FfiConverterStringINSTANCE.Lift(GoRustBuffer{
			inner: domain,
		}),
		FfiConverterStringINSTANCE.Lift(GoRustBuffer{
			inner: tier,
		}),
	)

}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod9
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod9(uniffiHandle C.uint64_t, uniffiOutReturn *C.void, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterLaunchPersistenceINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	uniffiObj.DeletePendingInvite()

}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod10
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod10(uniffiHandle C.uint64_t, uniffiOutReturn *C.RustBuffer, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterLaunchPersistenceINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	res :=
		uniffiObj.LoadReachIpv4()

	*uniffiOutReturn = FfiConverterOptionalStringINSTANCE.Lower(res)
}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod11
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod11(uniffiHandle C.uint64_t, uniffiOutReturn *C.void, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterLaunchPersistenceINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	uniffiObj.DeleteReachIpv4()

}

var UniffiVTableCallbackInterfaceLaunchPersistenceINSTANCE = C.UniffiVTableCallbackInterfaceLaunchPersistence{
	uniffiFree:                (C.UniffiCallbackInterfaceFree)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceFree),
	uniffiClone:               (C.UniffiCallbackInterfaceClone)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceClone),
	accountIndexRefusal:       (C.UniffiCallbackInterfaceLaunchPersistenceMethod0)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod0),
	loadIdentity:              (C.UniffiCallbackInterfaceLaunchPersistenceMethod1)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod1),
	loadNestUrl:               (C.UniffiCallbackInterfaceLaunchPersistenceMethod2)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod2),
	loadPendingInvite:         (C.UniffiCallbackInterfaceLaunchPersistenceMethod3)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod3),
	loadAwaitingDns:           (C.UniffiCallbackInterfaceLaunchPersistenceMethod4)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod4),
	loadPendingFactoryReset:   (C.UniffiCallbackInterfaceLaunchPersistenceMethod5)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod5),
	savePendingFactoryReset:   (C.UniffiCallbackInterfaceLaunchPersistenceMethod6)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod6),
	deletePendingFactoryReset: (C.UniffiCallbackInterfaceLaunchPersistenceMethod7)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod7),
	saveAuthenticated:         (C.UniffiCallbackInterfaceLaunchPersistenceMethod8)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod8),
	deletePendingInvite:       (C.UniffiCallbackInterfaceLaunchPersistenceMethod9)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod9),
	loadReachIpv4:             (C.UniffiCallbackInterfaceLaunchPersistenceMethod10)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod10),
	deleteReachIpv4:           (C.UniffiCallbackInterfaceLaunchPersistenceMethod11)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceMethod11),
}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceFree
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceFree(handle C.uint64_t) {
	FfiConverterLaunchPersistenceINSTANCE.handleMap.remove(uint64(handle))
}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceClone
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfaceLaunchPersistenceClone(handle C.uint64_t) C.uint64_t {
	val, ok := FfiConverterLaunchPersistenceINSTANCE.handleMap.tryGet(uint64(handle))
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}
	return C.uint64_t(FfiConverterLaunchPersistenceINSTANCE.handleMap.insert(val))
}

func (c FfiConverterLaunchPersistence) register() {
	C.uniffi_fauna_launch_machine_fn_init_callback_vtable_launchpersistence(&UniffiVTableCallbackInterfaceLaunchPersistenceINSTANCE)
}

// Writer for the **pending-provision slot** — the onboarding wizard's one
// durable side effect outside its own state
// (`docs/goal/behavior/onboarding.md` § 6 *The pending-provision slot*).
//
// Separate from [`LaunchPersistence`] rather than a method on it, because the
// contract is genuinely different in the one way that matters: every
// `LaunchPersistence` write is addressed by *the session's* account (the bound
// one, or whoever is active), and during the first-run wizard there may not be
// one yet. This write is addressed by **the identity being onboarded**, which
// the wizard knows and the session does not — so the secret comes in as an
// argument and the implementation registers the account if it has to.
//
// One implementation (`fauna_client_accounts::persist_awaiting_dns`, over the
// per-actor registry slot) serves all seven apps; nothing here is per-app glue.
type PendingProvisionStore interface {
	// Write (or **complete** — see the note on the shared implementation) the
	// awaiting-dns slot for `secret_hex`'s identity, registering it as an
	// account first if it is not one yet, and return the record **read back**
	// from the store.
	//
	// The read-back is the whole contract, and it is not defensive tidying:
	// the platform stores swallow failures (`SecretStore::set` is infallible
	// by signature on every platform, and the wasm shim drops a JS
	// `QuotaExceededError` on the floor), so "saved" is a claim to VERIFY.
	// Return `None` when the row cannot be read back — the caller must then
	// treat the code as unpersisted and refuse to build a box with it.
	SaveAwaitingDns(secretHex string, record AwaitingDnsRecord) *AwaitingDnsRecord
	// Clear the awaiting-dns slot of `secret_hex`'s identity — the durable half
	// of the "Almost ready" surface's explicit exit (`onboarding-provisioning.md`
	// § "Almost ready" surface → *Exit*).
	//
	// Addressed by the identity being onboarded, exactly as
	// [`Self::save_awaiting_dns`] is, and for the same reason: on an append run
	// the active account is a different one, and clearing *its* slot would leave
	// the abandoned box's slot to route every relaunch back onto the surface.
	// A no-op when the identity is not registered (no slot can exist) or holds
	// no slot.
	ClearAwaitingDns(secretHex string)
}

// Writer for the **pending-provision slot** — the onboarding wizard's one
// durable side effect outside its own state
// (`docs/goal/behavior/onboarding.md` § 6 *The pending-provision slot*).
//
// Separate from [`LaunchPersistence`] rather than a method on it, because the
// contract is genuinely different in the one way that matters: every
// `LaunchPersistence` write is addressed by *the session's* account (the bound
// one, or whoever is active), and during the first-run wizard there may not be
// one yet. This write is addressed by **the identity being onboarded**, which
// the wizard knows and the session does not — so the secret comes in as an
// argument and the implementation registers the account if it has to.
//
// One implementation (`fauna_client_accounts::persist_awaiting_dns`, over the
// per-actor registry slot) serves all seven apps; nothing here is per-app glue.
type PendingProvisionStoreImpl struct {
	ffiObject FfiObject
}

// Write (or **complete** — see the note on the shared implementation) the
// awaiting-dns slot for `secret_hex`'s identity, registering it as an
// account first if it is not one yet, and return the record **read back**
// from the store.
//
// The read-back is the whole contract, and it is not defensive tidying:
// the platform stores swallow failures (`SecretStore::set` is infallible
// by signature on every platform, and the wasm shim drops a JS
// `QuotaExceededError` on the floor), so "saved" is a claim to VERIFY.
// Return `None` when the row cannot be read back — the caller must then
// treat the code as unpersisted and refuse to build a box with it.
func (_self *PendingProvisionStoreImpl) SaveAwaitingDns(secretHex string, record AwaitingDnsRecord) *AwaitingDnsRecord {
	_pointer := _self.ffiObject.incrementPointer("PendingProvisionStore")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalAwaitingDnsRecordINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_launch_machine_fn_method_pendingprovisionstore_save_awaiting_dns(
				_pointer, FfiConverterStringINSTANCE.Lower(secretHex), FfiConverterAwaitingDnsRecordINSTANCE.Lower(record), _uniffiStatus),
		}
	}))
}

// Clear the awaiting-dns slot of `secret_hex`'s identity — the durable half
// of the "Almost ready" surface's explicit exit (`onboarding-provisioning.md`
// § "Almost ready" surface → *Exit*).
//
// Addressed by the identity being onboarded, exactly as
// [`Self::save_awaiting_dns`] is, and for the same reason: on an append run
// the active account is a different one, and clearing *its* slot would leave
// the abandoned box's slot to route every relaunch back onto the surface.
// A no-op when the identity is not registered (no slot can exist) or holds
// no slot.
func (_self *PendingProvisionStoreImpl) ClearAwaitingDns(secretHex string) {
	_pointer := _self.ffiObject.incrementPointer("PendingProvisionStore")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_launch_machine_fn_method_pendingprovisionstore_clear_awaiting_dns(
			_pointer, FfiConverterStringINSTANCE.Lower(secretHex), _uniffiStatus)
		return false
	})
}
func (object *PendingProvisionStoreImpl) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterPendingProvisionStore struct {
	handleMap *concurrentHandleMap[PendingProvisionStore]
}

var FfiConverterPendingProvisionStoreINSTANCE = FfiConverterPendingProvisionStore{
	handleMap: newConcurrentHandleMap[PendingProvisionStore](),
}

func (c FfiConverterPendingProvisionStore) Lift(handle C.uint64_t) PendingProvisionStore {
	if uint64(handle)&1 == 0 {
		// Rust-generated handle (even), construct a new object wrapping the handle
		result := &PendingProvisionStoreImpl{
			newFfiObject(
				handle,
				func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
					return C.uniffi_fauna_launch_machine_fn_clone_pendingprovisionstore(handle, status)
				},
				func(handle C.uint64_t, status *C.RustCallStatus) {
					C.uniffi_fauna_launch_machine_fn_free_pendingprovisionstore(handle, status)
				},
			),
		}
		runtime.SetFinalizer(result, (*PendingProvisionStoreImpl).Destroy)
		return result
	} else {
		// Go-generated handle (odd), retrieve from the handle map
		val, ok := c.handleMap.tryGet(uint64(handle))
		if !ok {
			panic(fmt.Errorf("no callback in handle map: %d", handle))
		}
		c.handleMap.remove(uint64(handle))
		return val
	}
}

func (c FfiConverterPendingProvisionStore) Read(reader io.Reader) PendingProvisionStore {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterPendingProvisionStore) Lower(value PendingProvisionStore) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	if val, ok := value.(*PendingProvisionStoreImpl); ok {
		// Rust-backed object, clone the handle
		handle := val.ffiObject.incrementPointer("PendingProvisionStore")
		defer val.ffiObject.decrementPointer()
		return handle
	} else {
		// Go-backed object, insert into handle map
		return C.uint64_t(c.handleMap.insert(value))
	}
}

func (c FfiConverterPendingProvisionStore) Write(writer io.Writer, value PendingProvisionStore) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalPendingProvisionStore(handle uint64) PendingProvisionStore {
	return FfiConverterPendingProvisionStoreINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalPendingProvisionStore(value PendingProvisionStore) uint64 {
	return uint64(FfiConverterPendingProvisionStoreINSTANCE.Lower(value))
}

type FfiDestroyerPendingProvisionStore struct{}

func (_ FfiDestroyerPendingProvisionStore) Destroy(value PendingProvisionStore) {
	if val, ok := value.(*PendingProvisionStoreImpl); ok {
		val.Destroy()
	}
}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfacePendingProvisionStoreMethod0
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfacePendingProvisionStoreMethod0(uniffiHandle C.uint64_t, secretHex C.RustBuffer, record C.RustBuffer, uniffiOutReturn *C.RustBuffer, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterPendingProvisionStoreINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	res :=
		uniffiObj.SaveAwaitingDns(
			FfiConverterStringINSTANCE.Lift(GoRustBuffer{
				inner: secretHex,
			}),
			FfiConverterAwaitingDnsRecordINSTANCE.Lift(GoRustBuffer{
				inner: record,
			}),
		)

	*uniffiOutReturn = FfiConverterOptionalAwaitingDnsRecordINSTANCE.Lower(res)
}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfacePendingProvisionStoreMethod1
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfacePendingProvisionStoreMethod1(uniffiHandle C.uint64_t, secretHex C.RustBuffer, uniffiOutReturn *C.void, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterPendingProvisionStoreINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	uniffiObj.ClearAwaitingDns(
		FfiConverterStringINSTANCE.Lift(GoRustBuffer{
			inner: secretHex,
		}),
	)

}

var UniffiVTableCallbackInterfacePendingProvisionStoreINSTANCE = C.UniffiVTableCallbackInterfacePendingProvisionStore{
	uniffiFree:       (C.UniffiCallbackInterfaceFree)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfacePendingProvisionStoreFree),
	uniffiClone:      (C.UniffiCallbackInterfaceClone)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfacePendingProvisionStoreClone),
	saveAwaitingDns:  (C.UniffiCallbackInterfacePendingProvisionStoreMethod0)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfacePendingProvisionStoreMethod0),
	clearAwaitingDns: (C.UniffiCallbackInterfacePendingProvisionStoreMethod1)(C.fauna_launch_machine_persistence_cgo_dispatchCallbackInterfacePendingProvisionStoreMethod1),
}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfacePendingProvisionStoreFree
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfacePendingProvisionStoreFree(handle C.uint64_t) {
	FfiConverterPendingProvisionStoreINSTANCE.handleMap.remove(uint64(handle))
}

//export fauna_launch_machine_persistence_cgo_dispatchCallbackInterfacePendingProvisionStoreClone
func fauna_launch_machine_persistence_cgo_dispatchCallbackInterfacePendingProvisionStoreClone(handle C.uint64_t) C.uint64_t {
	val, ok := FfiConverterPendingProvisionStoreINSTANCE.handleMap.tryGet(uint64(handle))
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}
	return C.uint64_t(FfiConverterPendingProvisionStoreINSTANCE.handleMap.insert(val))
}

func (c FfiConverterPendingProvisionStore) register() {
	C.uniffi_fauna_launch_machine_fn_init_callback_vtable_pendingprovisionstore(&UniffiVTableCallbackInterfacePendingProvisionStoreINSTANCE)
}

// One row in the awaiting-manual-dns slot — the deferred-DNS analogue of
// [`PendingInviteRecord`]. Field names match
// `docs/goal/behavior/onboarding.md` § Long-term store contract.
//
// `handle` is **required**: the eventual `LoggedIn` outcome carries it and it
// is not derivable from `nest_url` alone, and the wizard's
// `seed_awaiting_manual_dns(nest_url, handle, dns_records, claim_code)` seeder
// takes it. (Every pre-2026-07-11 per-app slot omitted it and so could not
// satisfy that seeder — see the goal doc's § Implementation status today.)
type AwaitingDnsRecord struct {
	NestUrl string
	Handle  string
	// Opaque to the launch machine: `serde_json::to_string(&dns_records)` over
	// the wizard's `Vec<DnsRecordPlain>`. Carried through to the wizard's
	// `seed_awaiting_manual_dns` when the launch flow lands on
	// `AwaitingManualDns`, exactly as `status_json` is for the invite slot.
	DnsRecordsJson string
	ClaimCode      string
	// The box's public IPv4 — the **reach address** every link of the
	// post-build chain dials while the domain's A record is still
	// propagating and the box holds no cert for a name it learns at the
	// claim (`docs/goal/behavior/onboarding.md` § 6 *Reaching the box*).
	//
	// `None` in three honest cases, and a reader must handle all three: a
	// record written before this field existed (`#[serde(default)]`), the
	// deferred-DNS exit's own record (that box is reached by the domain the
	// user is about to point at it), and the pending-provision slot in the
	// window between its pre-`create_server` write and the moment
	// `create_server` returns — which is precisely the window where the box
	// does not exist yet, so there is no address to hold.
	ReachIpv4 *string
	// The identity the box was **built with** — the `nest_actor_id` (64 hex)
	// derived from the deployment seed this client injected into its
	// cloud-init — and therefore the first-contact root every pre-identity
	// dial of this box verifies against (`docs/goal/architecture/security.md`
	// § Transport trust, the *Client-provisioned box* row).
	//
	// Persisted so the root survives what the machine's memory does not: a
	// Retry after a failed run reuses it instead of expecting a freshly-minted
	// identity of a box that was built with the old one, and a relaunch onto
	// the "Almost ready" surface re-holds it before its first poll
	// (`onboarding.md` § 6 *The pending-provision slot*). It is the derived
	// **public** key only — the seed itself is never persisted pre-claim
	// (`nest/box-recovery.md` § Mechanism); a box that has to be re-created
	// gets a fresh seed, and this field is rewritten with it.
	//
	// `None` (`#[serde(default)]`) for a slot whose
	// box was found already at the provider without this client ever having
	// built it — a reader with no identity to hold falls back to the
	// DNS-`self=`/TOFU ladder.
	NestActorId *string
}

func (r *AwaitingDnsRecord) Destroy() {
	FfiDestroyerString{}.Destroy(r.NestUrl)
	FfiDestroyerString{}.Destroy(r.Handle)
	FfiDestroyerString{}.Destroy(r.DnsRecordsJson)
	FfiDestroyerString{}.Destroy(r.ClaimCode)
	FfiDestroyerOptionalString{}.Destroy(r.ReachIpv4)
	FfiDestroyerOptionalString{}.Destroy(r.NestActorId)
}

type FfiConverterAwaitingDnsRecord struct{}

var FfiConverterAwaitingDnsRecordINSTANCE = FfiConverterAwaitingDnsRecord{}

func (c FfiConverterAwaitingDnsRecord) Lift(rb RustBufferI) AwaitingDnsRecord {
	return LiftFromRustBuffer[AwaitingDnsRecord](c, rb)
}

func (c FfiConverterAwaitingDnsRecord) Read(reader io.Reader) AwaitingDnsRecord {
	return AwaitingDnsRecord{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterAwaitingDnsRecord) Lower(value AwaitingDnsRecord) C.RustBuffer {
	return LowerIntoRustBuffer[AwaitingDnsRecord](c, value)
}

func (c FfiConverterAwaitingDnsRecord) LowerExternal(value AwaitingDnsRecord) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AwaitingDnsRecord](c, value))
}

func (c FfiConverterAwaitingDnsRecord) Write(writer io.Writer, value AwaitingDnsRecord) {
	FfiConverterStringINSTANCE.Write(writer, value.NestUrl)
	FfiConverterStringINSTANCE.Write(writer, value.Handle)
	FfiConverterStringINSTANCE.Write(writer, value.DnsRecordsJson)
	FfiConverterStringINSTANCE.Write(writer, value.ClaimCode)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.ReachIpv4)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.NestActorId)
}

type FfiDestroyerAwaitingDnsRecord struct{}

func (_ FfiDestroyerAwaitingDnsRecord) Destroy(value AwaitingDnsRecord) {
	value.Destroy()
}

// The account identity a successful silent challenge confirmed — the
// `handle`/`domain`/`tier` triple `VerifyReply` carries.
//
// `domain` is the **handle's** domain as the nest reports it, never the nest
// URL's host: a nest serves handles on domains that need not be its hostname,
// and composing an address from the dialed host is the exact bug
// `conversations.md` § *Self-address: live, never baked* forbids (it
// mis-routes same-nest vs. cross-nest, not just the `From:` label).
type LaunchIdentity struct {
	// The actor's handle, bare (no `@domain`). Empty when the account has none
	// yet — an app pairs empty with `domain` into no address at all, never into
	// the `"@nest.example"` shape the § names as forbidden.
	Handle string
	// The handle's domain.
	Domain string
	// The actor's tier name.
	Tier string
}

func (r *LaunchIdentity) Destroy() {
	FfiDestroyerString{}.Destroy(r.Handle)
	FfiDestroyerString{}.Destroy(r.Domain)
	FfiDestroyerString{}.Destroy(r.Tier)
}

type FfiConverterLaunchIdentity struct{}

var FfiConverterLaunchIdentityINSTANCE = FfiConverterLaunchIdentity{}

func (c FfiConverterLaunchIdentity) Lift(rb RustBufferI) LaunchIdentity {
	return LiftFromRustBuffer[LaunchIdentity](c, rb)
}

func (c FfiConverterLaunchIdentity) Read(reader io.Reader) LaunchIdentity {
	return LaunchIdentity{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterLaunchIdentity) Lower(value LaunchIdentity) C.RustBuffer {
	return LowerIntoRustBuffer[LaunchIdentity](c, value)
}

func (c FfiConverterLaunchIdentity) LowerExternal(value LaunchIdentity) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[LaunchIdentity](c, value))
}

func (c FfiConverterLaunchIdentity) Write(writer io.Writer, value LaunchIdentity) {
	FfiConverterStringINSTANCE.Write(writer, value.Handle)
	FfiConverterStringINSTANCE.Write(writer, value.Domain)
	FfiConverterStringINSTANCE.Write(writer, value.Tier)
}

type FfiDestroyerLaunchIdentity struct{}

func (_ FfiDestroyerLaunchIdentity) Destroy(value LaunchIdentity) {
	value.Destroy()
}

// Top-level observable shape. Cheap to clone.
type LaunchSnapshot struct {
	Phase LaunchPhase
	Token TokenStatus
	// Human-readable reason for the most recent failure. Clients display
	// it in error surfaces; mapping to localized i18n keys is left to
	// the client.
	LastError *string
	// Set when the launch flow was refused with `fauna.auth.superseded`: the
	// identity was succeeded and the account belongs to this actor id (64-hex)
	// now (`docs/goal/behavior/identity-succession.md` § Propagation → *Own
	// device fleet*). `None` in every other case.
	//
	// **Claimed, not proven.** This is whatever the refusal named; a client
	// verifies it against the registration chain
	// (`fauna_client_recovery::resolve_successor`) before presenting it as
	// fact, because the nest is enforcer and distributor, never authorizer.
	//
	// An **additive side channel** rather than a `LaunchPhase` variant, on
	// purpose — `State::Superseded`'s docs carry the full reasoning. The phase
	// is `Offline { transient: false }`, so an app that never reads this field
	// still stops retrying and still shows `last_error`; reading it is what
	// upgrades a dead end into "import the new identity".
	SupersededSuccessor *string
	// `true` when the current `LaunchPhase::IdentityChanged` is rotation-chain
	// **fork evidence** (`box-recovery.md` § Client acceptance): the box's
	// served rotation history contradicts the one this client accepted, so
	// there is **no re-trust on that surface** — the machine refuses
	// `trust_nest_identity()`, and an app reading this field hides/disables
	// the trust affordance and names the fork. `false` in every other case,
	// including the ordinary changed/withdrawn warnings (whose explicit
	// re-trust is unchanged).
	//
	// An **additive field** rather than a `LaunchPhase` field for the same
	// exhaustive-switch reason as `superseded_successor` (`State::Superseded`'s
	// docs). An app that never reads it still blocks (the machine-side refusal
	// is the security boundary); reading it is what upgrades the surface from
	// "a trust button that refuses" to the honest fork warning.
	IdentityFork bool
	// The account identity the nest confirmed on the most recent successful
	// silent challenge — `None` until one resolves.
	//
	// This is the **live identity channel** every native app shares
	// (`docs/goal/ui/conversations.md` § State & data shape → *Self-address:
	// live, never baked*: "a client calls the setter from the one place
	// identity state lands"). The machine already learned all three fields from
	// the `VerifyReply` and wrote them to the long-term store via
	// `LaunchPersistence::save_authenticated` — but a store write is not an
	// event, so an app observing only `on_changed()` had no way to notice the
	// identity resolving or changing. Surfacing it here is what lets an app
	// push `<handle>@<domain>` into `ConversationsSession::set_self_address`
	// (and refresh its own caches) the moment it lands, instead of assembling
	// an address from a cache written on some earlier run.
	//
	// **`None` is not an error state** — it is "not resolved yet", and per the
	// same § identity resolution must never delay conversation delivery: the
	// app builds its session immediately, renders whatever it cached for
	// "Welcome back", and refuses a send locally until a real address arrives.
	// A token refresh does not clear it: a refresh ignores its verify reply's
	// metadata (the launch owns the cached identity), so the last confirmed one
	// stands.
	Identity *LaunchIdentity
	// Set when the account index at `fauna/index` is present but this build
	// cannot use it (`version-compatibility.md` § 5 item 9). `None` in every
	// other case, including a fresh install with no index at all.
	//
	// An **additive side channel** rather than a `LaunchPhase` variant, on the
	// `superseded_successor` pattern above and for the same reason: the phase
	// is `Offline { transient: false }`, so an app that never reads this field
	// still stops retrying and still shows `last_error`, while reading it is
	// what upgrades a dead end into the right offer — "update the app" for the
	// version case, and for the malformed case the only case that may offer
	// the documented floor.
	//
	// Its presence is also what keeps the user **out of fresh onboarding**:
	// on either verdict the registry answers no session account, so without
	// this the launch machine reads the install as identity-less and routes to
	// `WizardAt { IdentityChoice }` — offering to make a new identity to
	// someone whose accounts are sitting intact behind an unparsed blob.
	AccountIndexRefusal *AccountIndexRefusal
	// `true` when a nest this app had **signed in to before** — the stored
	// identity + `nest_url` the silent-challenge row runs on — answered the
	// opaque `fauna.auth.not_registered`, and the nest is claimed
	// (`docs/goal/behavior/onboarding.md` § App-launch routing → *the
	// previously-signed-in row*; `login.md` § Silent Challenge). The nest no
	// longer signs this identity in: suspended, or removed — the app cannot
	// tell, by design (no suspended-vs-unregistered oracle on the wire), and
	// the copy asserts neither. Set by the launch arm and by the mid-session
	// refresh arm alike (`security.md` § Post-auth surfacing), `false` in
	// every other case, including an unregistered identity on an
	// **unclaimed** nest (that is the claim-code row) and the fresh-install
	// rows that never reach verify.
	//
	// An **additive side channel** on the `account_index_refusal` pattern
	// above: the phase is `Offline { transient: false }` with the localized
	// `onboarding.launch.sign_in_refused` in `last_error`, so an app that
	// never reads this field still stops routing into the invite wizard and
	// shows the honest sentence; reading it is what upgrades that dead end
	// into the `launch_sign_in_refused` surface — the same copy, plus
	// **Retry** (the one terminal offline whose retry the machine honours,
	// because the admin's restore is a button on *their* app and a retry is
	// the user's way back in) and "Use a different nest".
	SignInRefused bool
	// The unlock time (Unix seconds) when the machine is parked on
	// `fauna.auth.account_locked` (`devices.md` § The locked state); `None` in
	// every other case.
	//
	// An **additive side channel** on the `superseded_successor` pattern: the
	// phase is `Offline { transient: false }` with a time-free localized
	// `onboarding.launch.account_locked` in `last_error`, so an app that never
	// reads this field still stops retrying and shows an honest sentence;
	// reading it is what upgrades the dead end into the locked surface (the
	// unlock time through the shared `format_unix_local`).
	LockedUntilSecs *uint64
}

func (r *LaunchSnapshot) Destroy() {
	FfiDestroyerLaunchPhase{}.Destroy(r.Phase)
	FfiDestroyerTokenStatus{}.Destroy(r.Token)
	FfiDestroyerOptionalString{}.Destroy(r.LastError)
	FfiDestroyerOptionalString{}.Destroy(r.SupersededSuccessor)
	FfiDestroyerBool{}.Destroy(r.IdentityFork)
	FfiDestroyerOptionalLaunchIdentity{}.Destroy(r.Identity)
	FfiDestroyerOptionalAccountIndexRefusal{}.Destroy(r.AccountIndexRefusal)
	FfiDestroyerBool{}.Destroy(r.SignInRefused)
	FfiDestroyerOptionalUint64{}.Destroy(r.LockedUntilSecs)
}

type FfiConverterLaunchSnapshot struct{}

var FfiConverterLaunchSnapshotINSTANCE = FfiConverterLaunchSnapshot{}

func (c FfiConverterLaunchSnapshot) Lift(rb RustBufferI) LaunchSnapshot {
	return LiftFromRustBuffer[LaunchSnapshot](c, rb)
}

func (c FfiConverterLaunchSnapshot) Read(reader io.Reader) LaunchSnapshot {
	return LaunchSnapshot{
		FfiConverterLaunchPhaseINSTANCE.Read(reader),
		FfiConverterTokenStatusINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterOptionalLaunchIdentityINSTANCE.Read(reader),
		FfiConverterOptionalAccountIndexRefusalINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterOptionalUint64INSTANCE.Read(reader),
	}
}

func (c FfiConverterLaunchSnapshot) Lower(value LaunchSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[LaunchSnapshot](c, value)
}

func (c FfiConverterLaunchSnapshot) LowerExternal(value LaunchSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[LaunchSnapshot](c, value))
}

func (c FfiConverterLaunchSnapshot) Write(writer io.Writer, value LaunchSnapshot) {
	FfiConverterLaunchPhaseINSTANCE.Write(writer, value.Phase)
	FfiConverterTokenStatusINSTANCE.Write(writer, value.Token)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.LastError)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.SupersededSuccessor)
	FfiConverterBoolINSTANCE.Write(writer, value.IdentityFork)
	FfiConverterOptionalLaunchIdentityINSTANCE.Write(writer, value.Identity)
	FfiConverterOptionalAccountIndexRefusalINSTANCE.Write(writer, value.AccountIndexRefusal)
	FfiConverterBoolINSTANCE.Write(writer, value.SignInRefused)
	FfiConverterOptionalUint64INSTANCE.Write(writer, value.LockedUntilSecs)
}

type FfiDestroyerLaunchSnapshot struct{}

func (_ FfiDestroyerLaunchSnapshot) Destroy(value LaunchSnapshot) {
	value.Destroy()
}

// One row in the pending-factory-reset slot — the crash-atomic decision point
// for `fauna.admin.factory_reset` (gap CR-1,
// `docs/goal/architecture/nest/common.md` § Client-state recoverability).
//
// The post-reset claim code used to exist *only* in the synchronous
// `FactoryResetReply`, so a client SIGKILL'd between dispatch and reply-render
// lost it with no client able to learn it — the box landed at the recovery
// floor (fresh/unclaimed) but un-claimable. The client therefore mints the code
// and writes this row **before** dispatching, then pins it via
// `FactoryResetRequest.new_claim_code`; a relaunch resumes the pre-filled claim
// from the slot. Write it only through
// [`mint_and_persist_pending_factory_reset`], which makes the wrong ordering
// unrepresentable: the code cannot be obtained without having been persisted.
type PendingFactoryResetRecord struct {
	NestUrl string
	Handle  string
	// The code the client minted and pinned onto the reset request. The nest
	// honors a pinned code verbatim (trimmed), so this is the code the wiped
	// box will boot with.
	ClaimCode string
	// Unix seconds at mint time. The boot reconcile's `Claimed` arm reads a
	// probe within [`FACTORY_RESET_CLAIM_GRACE_SECS`] of this as
	// "reset in flight" (the box has staged the wipe but not yet executed it —
	// it still answers `Claimed` for a moment after the dispatch) and HONORS
	// the slot instead of clearing it; without this the machine's own CR-2
	// stale-slot reconcile destroys the freshly-minted code in that window —
	// CR-1 data loss through CR-2's door (measured on web, 2026-07-16).
	// Required: the one writer ([`mint_and_persist_pending_factory_reset`])
	// always stamps it, and a record without it does not parse (the adapter
	// reads an unparseable slot as no slot).
	MintedAtSecs uint64
}

func (r *PendingFactoryResetRecord) Destroy() {
	FfiDestroyerString{}.Destroy(r.NestUrl)
	FfiDestroyerString{}.Destroy(r.Handle)
	FfiDestroyerString{}.Destroy(r.ClaimCode)
	FfiDestroyerUint64{}.Destroy(r.MintedAtSecs)
}

type FfiConverterPendingFactoryResetRecord struct{}

var FfiConverterPendingFactoryResetRecordINSTANCE = FfiConverterPendingFactoryResetRecord{}

func (c FfiConverterPendingFactoryResetRecord) Lift(rb RustBufferI) PendingFactoryResetRecord {
	return LiftFromRustBuffer[PendingFactoryResetRecord](c, rb)
}

func (c FfiConverterPendingFactoryResetRecord) Read(reader io.Reader) PendingFactoryResetRecord {
	return PendingFactoryResetRecord{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
	}
}

func (c FfiConverterPendingFactoryResetRecord) Lower(value PendingFactoryResetRecord) C.RustBuffer {
	return LowerIntoRustBuffer[PendingFactoryResetRecord](c, value)
}

func (c FfiConverterPendingFactoryResetRecord) LowerExternal(value PendingFactoryResetRecord) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[PendingFactoryResetRecord](c, value))
}

func (c FfiConverterPendingFactoryResetRecord) Write(writer io.Writer, value PendingFactoryResetRecord) {
	FfiConverterStringINSTANCE.Write(writer, value.NestUrl)
	FfiConverterStringINSTANCE.Write(writer, value.Handle)
	FfiConverterStringINSTANCE.Write(writer, value.ClaimCode)
	FfiConverterUint64INSTANCE.Write(writer, value.MintedAtSecs)
}

type FfiDestroyerPendingFactoryResetRecord struct{}

func (_ FfiDestroyerPendingFactoryResetRecord) Destroy(value PendingFactoryResetRecord) {
	value.Destroy()
}

// One row in the pending-invite slot. Field names match
// `docs/goal/behavior/onboarding.md` § Long-term store contract.
type PendingInviteRecord struct {
	NestUrl   string
	Handle    string
	RequestId string
	// Opaque to the launch machine. Carried through to the wizard's
	// `seed_pending_invite` when the launch flow lands on `InviteRequest`.
	StatusJson string
}

func (r *PendingInviteRecord) Destroy() {
	FfiDestroyerString{}.Destroy(r.NestUrl)
	FfiDestroyerString{}.Destroy(r.Handle)
	FfiDestroyerString{}.Destroy(r.RequestId)
	FfiDestroyerString{}.Destroy(r.StatusJson)
}

type FfiConverterPendingInviteRecord struct{}

var FfiConverterPendingInviteRecordINSTANCE = FfiConverterPendingInviteRecord{}

func (c FfiConverterPendingInviteRecord) Lift(rb RustBufferI) PendingInviteRecord {
	return LiftFromRustBuffer[PendingInviteRecord](c, rb)
}

func (c FfiConverterPendingInviteRecord) Read(reader io.Reader) PendingInviteRecord {
	return PendingInviteRecord{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterPendingInviteRecord) Lower(value PendingInviteRecord) C.RustBuffer {
	return LowerIntoRustBuffer[PendingInviteRecord](c, value)
}

func (c FfiConverterPendingInviteRecord) LowerExternal(value PendingInviteRecord) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[PendingInviteRecord](c, value))
}

func (c FfiConverterPendingInviteRecord) Write(writer io.Writer, value PendingInviteRecord) {
	FfiConverterStringINSTANCE.Write(writer, value.NestUrl)
	FfiConverterStringINSTANCE.Write(writer, value.Handle)
	FfiConverterStringINSTANCE.Write(writer, value.RequestId)
	FfiConverterStringINSTANCE.Write(writer, value.StatusJson)
}

type FfiDestroyerPendingInviteRecord struct{}

func (_ FfiDestroyerPendingInviteRecord) Destroy(value PendingInviteRecord) {
	value.Destroy()
}

// Why this build cannot read the account index at `fauna/index`, when it is
// present but unusable — the two verdicts `fauna-client-accounts` draws, in
// the shape the launch seam and the seven apps consume
// (`version-compatibility.md` § 5 item 9; owner of the verdicts themselves is
// that crate's `AccountError`).
//
// The distinction is the whole point: the two have **opposite remedies**, so
// an app that collapses them tells half its users to do something that cannot
// work. Neither ever licenses a rewrite — a blob nothing here can parse is
// not thereby known to name no accounts (I1) — so this type carries no
// "repair" arm and never will.
type AccountIndexRefusal interface {
	Destroy()
}

// A newer build wrote the index and said so in a stamp this build could
// read. **The accounts are intact**; updating the app brings them back,
// and nothing else will. The numbers are the ones actually read off the
// blob, never invented.
type AccountIndexRefusalNewerBuild struct {
	IndexV   uint16
	IndexMin uint16
	BinV     uint16
}

func (e AccountIndexRefusalNewerBuild) Destroy() {
	FfiDestroyerUint16{}.Destroy(e.IndexV)
	FfiDestroyerUint16{}.Destroy(e.IndexMin)
	FfiDestroyerUint16{}.Destroy(e.BinV)
}

// The index is present, unparseable, and carries no readable stamp
// either — so nothing about a newer build explains it and **updating
// cannot help**. The only route forward is the documented client-side
// floor (`long-term-store.md` § Cleanup contract), whose own residual for
// a corrupt index an app must state rather than imply.
type AccountIndexRefusalMalformed struct {
}

func (e AccountIndexRefusalMalformed) Destroy() {
}

type FfiConverterAccountIndexRefusal struct{}

var FfiConverterAccountIndexRefusalINSTANCE = FfiConverterAccountIndexRefusal{}

func (c FfiConverterAccountIndexRefusal) Lift(rb RustBufferI) AccountIndexRefusal {
	return LiftFromRustBuffer[AccountIndexRefusal](c, rb)
}

func (c FfiConverterAccountIndexRefusal) Lower(value AccountIndexRefusal) C.RustBuffer {
	return LowerIntoRustBuffer[AccountIndexRefusal](c, value)
}

func (c FfiConverterAccountIndexRefusal) LowerExternal(value AccountIndexRefusal) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AccountIndexRefusal](c, value))
}
func (FfiConverterAccountIndexRefusal) Read(reader io.Reader) AccountIndexRefusal {
	id := readInt32(reader)
	switch id {
	case 1:
		return AccountIndexRefusalNewerBuild{
			FfiConverterUint16INSTANCE.Read(reader),
			FfiConverterUint16INSTANCE.Read(reader),
			FfiConverterUint16INSTANCE.Read(reader),
		}
	case 2:
		return AccountIndexRefusalMalformed{}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterAccountIndexRefusal.Read()", id))
	}
}

func (FfiConverterAccountIndexRefusal) Write(writer io.Writer, value AccountIndexRefusal) {
	switch variant_value := value.(type) {
	case AccountIndexRefusalNewerBuild:
		writeInt32(writer, 1)
		FfiConverterUint16INSTANCE.Write(writer, variant_value.IndexV)
		FfiConverterUint16INSTANCE.Write(writer, variant_value.IndexMin)
		FfiConverterUint16INSTANCE.Write(writer, variant_value.BinV)
	case AccountIndexRefusalMalformed:
		writeInt32(writer, 2)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterAccountIndexRefusal.Write", value))
	}
}

type FfiDestroyerAccountIndexRefusal struct{}

func (_ FfiDestroyerAccountIndexRefusal) Destroy(value AccountIndexRefusal) {
	value.Destroy()
}

type LaunchError struct {
	err error
}

// Convenience method to turn *LaunchError into error
// Avoiding treating nil pointer as non nil error interface
func (err *LaunchError) AsError() error {
	if err == nil {
		return nil
	} else {
		return err
	}
}

func (err LaunchError) Error() string {
	return fmt.Sprintf("LaunchError: %s", err.err.Error())
}

func (err LaunchError) Unwrap() error {
	return err.err
}

// Err* are used for checking error type with `errors.Is`
var ErrLaunchErrorNetwork = fmt.Errorf("LaunchErrorNetwork")
var ErrLaunchErrorServerError = fmt.Errorf("LaunchErrorServerError")
var ErrLaunchErrorActorNotRegistered = fmt.Errorf("LaunchErrorActorNotRegistered")
var ErrLaunchErrorAccountLocked = fmt.Errorf("LaunchErrorAccountLocked")
var ErrLaunchErrorInvalidResponse = fmt.Errorf("LaunchErrorInvalidResponse")
var ErrLaunchErrorSignatureFailed = fmt.Errorf("LaunchErrorSignatureFailed")
var ErrLaunchErrorInvalidTransition = fmt.Errorf("LaunchErrorInvalidTransition")
var ErrLaunchErrorOther = fmt.Errorf("LaunchErrorOther")

// Variant structs
// Network / HTTP transport failure. `detail` is named `detail` (not
// `message`) so the UniFFI Kotlin generator doesn't collide with
// `Throwable.message` on the generated `LaunchException.Network`.
type LaunchErrorNetwork struct {
	Detail string
}

// Network / HTTP transport failure. `detail` is named `detail` (not
// `message`) so the UniFFI Kotlin generator doesn't collide with
// `Throwable.message` on the generated `LaunchException.Network`.
func NewLaunchErrorNetwork(
	detail string,
) *LaunchError {
	return &LaunchError{err: &LaunchErrorNetwork{
		Detail: detail}}
}

func (e LaunchErrorNetwork) destroy() {
	FfiDestroyerString{}.Destroy(e.Detail)
}

func (err LaunchErrorNetwork) Error() string {
	return fmt.Sprint("Network",
		": ",

		"Detail=",
		err.Detail,
	)
}

func (self LaunchErrorNetwork) Is(target error) bool {
	return target == ErrLaunchErrorNetwork
}

// Server returned a 4xx/5xx status the launch flow doesn't recognize.
type LaunchErrorServerError struct {
	Status uint16
	Detail string
}

// Server returned a 4xx/5xx status the launch flow doesn't recognize.
func NewLaunchErrorServerError(
	status uint16,
	detail string,
) *LaunchError {
	return &LaunchError{err: &LaunchErrorServerError{
		Status: status,
		Detail: detail}}
}

func (e LaunchErrorServerError) destroy() {
	FfiDestroyerUint16{}.Destroy(e.Status)
	FfiDestroyerString{}.Destroy(e.Detail)
}

func (err LaunchErrorServerError) Error() string {
	return fmt.Sprint("ServerError",
		": ",

		"Status=",
		err.Status,
		", ",
		"Detail=",
		err.Detail,
	)
}

func (self LaunchErrorServerError) Is(target error) bool {
	return target == ErrLaunchErrorServerError
}

// `/auth/verify` returned 404 "actor not registered" — the launch
// flow uses this to drop to the wizard at `invite_request`.
type LaunchErrorActorNotRegistered struct {
}

// `/auth/verify` returned 404 "actor not registered" — the launch
// flow uses this to drop to the wizard at `invite_request`.
func NewLaunchErrorActorNotRegistered() *LaunchError {
	return &LaunchError{err: &LaunchErrorActorNotRegistered{}}
}

func (e LaunchErrorActorNotRegistered) destroy() {
}

func (err LaunchErrorActorNotRegistered) Error() string {
	return fmt.Sprint("ActorNotRegistered")
}

func (self LaunchErrorActorNotRegistered) Is(target error) bool {
	return target == ErrLaunchErrorActorNotRegistered
}

// Account is locked (HTTP 423 from `/auth/token`). `locked_until_secs`
// is unix seconds; clients display a countdown.
type LaunchErrorAccountLocked struct {
	LockedUntilSecs uint64
}

// Account is locked (HTTP 423 from `/auth/token`). `locked_until_secs`
// is unix seconds; clients display a countdown.
func NewLaunchErrorAccountLocked(
	lockedUntilSecs uint64,
) *LaunchError {
	return &LaunchError{err: &LaunchErrorAccountLocked{
		LockedUntilSecs: lockedUntilSecs}}
}

func (e LaunchErrorAccountLocked) destroy() {
	FfiDestroyerUint64{}.Destroy(e.LockedUntilSecs)
}

func (err LaunchErrorAccountLocked) Error() string {
	return fmt.Sprint("AccountLocked",
		": ",

		"LockedUntilSecs=",
		err.LockedUntilSecs,
	)
}

func (self LaunchErrorAccountLocked) Is(target error) bool {
	return target == ErrLaunchErrorAccountLocked
}

// Server returned a body we couldn't parse (missing field, wrong shape).
type LaunchErrorInvalidResponse struct {
	Detail string
}

// Server returned a body we couldn't parse (missing field, wrong shape).
func NewLaunchErrorInvalidResponse(
	detail string,
) *LaunchError {
	return &LaunchError{err: &LaunchErrorInvalidResponse{
		Detail: detail}}
}

func (e LaunchErrorInvalidResponse) destroy() {
	FfiDestroyerString{}.Destroy(e.Detail)
}

func (err LaunchErrorInvalidResponse) Error() string {
	return fmt.Sprint("InvalidResponse",
		": ",

		"Detail=",
		err.Detail,
	)
}

func (self LaunchErrorInvalidResponse) Is(target error) bool {
	return target == ErrLaunchErrorInvalidResponse
}

// Local Ed25519 signature failed to construct (corrupt secret bytes,
// length mismatch, etc.).
type LaunchErrorSignatureFailed struct {
	Reason string
}

// Local Ed25519 signature failed to construct (corrupt secret bytes,
// length mismatch, etc.).
func NewLaunchErrorSignatureFailed(
	reason string,
) *LaunchError {
	return &LaunchError{err: &LaunchErrorSignatureFailed{
		Reason: reason}}
}

func (e LaunchErrorSignatureFailed) destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

func (err LaunchErrorSignatureFailed) Error() string {
	return fmt.Sprint("SignatureFailed",
		": ",

		"Reason=",
		err.Reason,
	)
}

func (self LaunchErrorSignatureFailed) Is(target error) bool {
	return target == ErrLaunchErrorSignatureFailed
}

// Caller invoked a transition that the current phase doesn't allow
// (e.g. `notify_401()` while in `Boot`).
type LaunchErrorInvalidTransition struct {
	From   string
	Reason string
}

// Caller invoked a transition that the current phase doesn't allow
// (e.g. `notify_401()` while in `Boot`).
func NewLaunchErrorInvalidTransition(
	from string,
	reason string,
) *LaunchError {
	return &LaunchError{err: &LaunchErrorInvalidTransition{
		From:   from,
		Reason: reason}}
}

func (e LaunchErrorInvalidTransition) destroy() {
	FfiDestroyerString{}.Destroy(e.From)
	FfiDestroyerString{}.Destroy(e.Reason)
}

func (err LaunchErrorInvalidTransition) Error() string {
	return fmt.Sprint("InvalidTransition",
		": ",

		"From=",
		err.From,
		", ",
		"Reason=",
		err.Reason,
	)
}

func (self LaunchErrorInvalidTransition) Is(target error) bool {
	return target == ErrLaunchErrorInvalidTransition
}

// Generic fallback. Field is `detail` for Kotlin-collision reasons.
type LaunchErrorOther struct {
	Detail string
}

// Generic fallback. Field is `detail` for Kotlin-collision reasons.
func NewLaunchErrorOther(
	detail string,
) *LaunchError {
	return &LaunchError{err: &LaunchErrorOther{
		Detail: detail}}
}

func (e LaunchErrorOther) destroy() {
	FfiDestroyerString{}.Destroy(e.Detail)
}

func (err LaunchErrorOther) Error() string {
	return fmt.Sprint("Other",
		": ",

		"Detail=",
		err.Detail,
	)
}

func (self LaunchErrorOther) Is(target error) bool {
	return target == ErrLaunchErrorOther
}

type FfiConverterLaunchError struct{}

var FfiConverterLaunchErrorINSTANCE = FfiConverterLaunchError{}

func (c FfiConverterLaunchError) Lift(eb RustBufferI) *LaunchError {
	return LiftFromRustBuffer[*LaunchError](c, eb)
}

func (c FfiConverterLaunchError) Lower(value *LaunchError) C.RustBuffer {
	return LowerIntoRustBuffer[*LaunchError](c, value)
}

func (c FfiConverterLaunchError) LowerExternal(value *LaunchError) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*LaunchError](c, value))
}

func (c FfiConverterLaunchError) Read(reader io.Reader) *LaunchError {
	errorID := readUint32(reader)

	switch errorID {
	case 1:
		return &LaunchError{&LaunchErrorNetwork{
			Detail: FfiConverterStringINSTANCE.Read(reader),
		}}
	case 2:
		return &LaunchError{&LaunchErrorServerError{
			Status: FfiConverterUint16INSTANCE.Read(reader),
			Detail: FfiConverterStringINSTANCE.Read(reader),
		}}
	case 3:
		return &LaunchError{&LaunchErrorActorNotRegistered{}}
	case 4:
		return &LaunchError{&LaunchErrorAccountLocked{
			LockedUntilSecs: FfiConverterUint64INSTANCE.Read(reader),
		}}
	case 5:
		return &LaunchError{&LaunchErrorInvalidResponse{
			Detail: FfiConverterStringINSTANCE.Read(reader),
		}}
	case 6:
		return &LaunchError{&LaunchErrorSignatureFailed{
			Reason: FfiConverterStringINSTANCE.Read(reader),
		}}
	case 7:
		return &LaunchError{&LaunchErrorInvalidTransition{
			From:   FfiConverterStringINSTANCE.Read(reader),
			Reason: FfiConverterStringINSTANCE.Read(reader),
		}}
	case 8:
		return &LaunchError{&LaunchErrorOther{
			Detail: FfiConverterStringINSTANCE.Read(reader),
		}}
	default:
		panic(fmt.Sprintf("Unknown error code %d in FfiConverterLaunchError.Read()", errorID))
	}
}

func (c FfiConverterLaunchError) Write(writer io.Writer, value *LaunchError) {
	switch variantValue := value.err.(type) {
	case *LaunchErrorNetwork:
		writeInt32(writer, 1)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Detail)
	case *LaunchErrorServerError:
		writeInt32(writer, 2)
		FfiConverterUint16INSTANCE.Write(writer, variantValue.Status)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Detail)
	case *LaunchErrorActorNotRegistered:
		writeInt32(writer, 3)
	case *LaunchErrorAccountLocked:
		writeInt32(writer, 4)
		FfiConverterUint64INSTANCE.Write(writer, variantValue.LockedUntilSecs)
	case *LaunchErrorInvalidResponse:
		writeInt32(writer, 5)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Detail)
	case *LaunchErrorSignatureFailed:
		writeInt32(writer, 6)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Reason)
	case *LaunchErrorInvalidTransition:
		writeInt32(writer, 7)
		FfiConverterStringINSTANCE.Write(writer, variantValue.From)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Reason)
	case *LaunchErrorOther:
		writeInt32(writer, 8)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Detail)
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiConverterLaunchError.Write", value))
	}
}

type FfiDestroyerLaunchError struct{}

func (_ FfiDestroyerLaunchError) Destroy(value *LaunchError) {
	switch variantValue := value.err.(type) {
	case LaunchErrorNetwork:
		variantValue.destroy()
	case LaunchErrorServerError:
		variantValue.destroy()
	case LaunchErrorActorNotRegistered:
		variantValue.destroy()
	case LaunchErrorAccountLocked:
		variantValue.destroy()
	case LaunchErrorInvalidResponse:
		variantValue.destroy()
	case LaunchErrorSignatureFailed:
		variantValue.destroy()
	case LaunchErrorInvalidTransition:
		variantValue.destroy()
	case LaunchErrorOther:
		variantValue.destroy()
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiDestroyerLaunchError.Destroy", value))
	}
}

// Coarse-grained phase the launch flow is in. Drives client UI:
// Boot/Hydrating → splash; SilentChallenge/Refreshing → spinner with
// optional cached metadata; Online → main app; Offline → retry surface;
// WizardAt → mount the OnboardingMachine at the given entry.
type LaunchPhase interface {
	Destroy()
}

// Initial state before the machine has done anything.
type LaunchPhaseBoot struct {
}

func (e LaunchPhaseBoot) Destroy() {
}

// Reading the long-term store to determine which branch to take.
type LaunchPhaseHydrating struct {
}

func (e LaunchPhaseHydrating) Destroy() {
}

// Existing-account fast path: silent challenge in flight (`/auth/challenge`
// then `/auth/verify`). `attempt` increments on retry.
type LaunchPhaseSilentChallenge struct {
	Attempt uint32
}

func (e LaunchPhaseSilentChallenge) Destroy() {
	FfiDestroyerUint32{}.Destroy(e.Attempt)
}

// Authenticated; a token refresh is in flight.
type LaunchPhaseRefreshing struct {
	Reason RefreshReason
}

func (e LaunchPhaseRefreshing) Destroy() {
	FfiDestroyerRefreshReason{}.Destroy(e.Reason)
}

// Authenticated; token is valid; the main app should render.
type LaunchPhaseOnline struct {
}

func (e LaunchPhaseOnline) Destroy() {
}

// Network or server failure prevented authentication. `transient: true`
// shows a retry indicator on the launch screen; `transient: false` is
// terminal (e.g. account locked) and the client surfaces it as such.
type LaunchPhaseOffline struct {
	Transient bool
}

func (e LaunchPhaseOffline) Destroy() {
	FfiDestroyerBool{}.Destroy(e.Transient)
}

// Long-term store says: drop into the onboarding wizard at this entry.
// The client mounts an `OnboardingMachine` and seeds it from its own
// persistence (the LaunchMachine doesn't carry wizard state).
type LaunchPhaseWizardAt struct {
	Entry LaunchWizardEntry
}

func (e LaunchPhaseWizardAt) Destroy() {
	FfiDestroyerLaunchWizardEntry{}.Destroy(e.Entry)
}

// The nest's **pinned deployment identity** changed — or a pinned nest
// could no longer prove any identity (`seen_hex: None`, the
// withdrawn/downgrade case). The SSH `known_hosts` model (security.md
// § Transport trust): auto-entry is BLOCKED; the client
// renders the `launch_identity_changed` warning surface
// (`nest-identity-changed-warning`) with two explicit ways out —
// "trust this nest" → [`LaunchMachine::trust_nest_identity`] (forget the
// pin, re-TOFU, re-run the silent challenge) and "use a different nest"
// → the wizard fallthrough. Never auto-repinned, never a retry loop.
// Fingerprints are hex `nest_actor_id`s for the warning's detail line;
// the nest's address comes from the client's own persistence.
type LaunchPhaseIdentityChanged struct {
	PinnedHex string
	SeenHex   *string
}

func (e LaunchPhaseIdentityChanged) Destroy() {
	FfiDestroyerString{}.Destroy(e.PinnedHex)
	FfiDestroyerOptionalString{}.Destroy(e.SeenHex)
}

type FfiConverterLaunchPhase struct{}

var FfiConverterLaunchPhaseINSTANCE = FfiConverterLaunchPhase{}

func (c FfiConverterLaunchPhase) Lift(rb RustBufferI) LaunchPhase {
	return LiftFromRustBuffer[LaunchPhase](c, rb)
}

func (c FfiConverterLaunchPhase) Lower(value LaunchPhase) C.RustBuffer {
	return LowerIntoRustBuffer[LaunchPhase](c, value)
}

func (c FfiConverterLaunchPhase) LowerExternal(value LaunchPhase) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[LaunchPhase](c, value))
}
func (FfiConverterLaunchPhase) Read(reader io.Reader) LaunchPhase {
	id := readInt32(reader)
	switch id {
	case 1:
		return LaunchPhaseBoot{}
	case 2:
		return LaunchPhaseHydrating{}
	case 3:
		return LaunchPhaseSilentChallenge{
			FfiConverterUint32INSTANCE.Read(reader),
		}
	case 4:
		return LaunchPhaseRefreshing{
			FfiConverterRefreshReasonINSTANCE.Read(reader),
		}
	case 5:
		return LaunchPhaseOnline{}
	case 6:
		return LaunchPhaseOffline{
			FfiConverterBoolINSTANCE.Read(reader),
		}
	case 7:
		return LaunchPhaseWizardAt{
			FfiConverterLaunchWizardEntryINSTANCE.Read(reader),
		}
	case 8:
		return LaunchPhaseIdentityChanged{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterOptionalStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterLaunchPhase.Read()", id))
	}
}

func (FfiConverterLaunchPhase) Write(writer io.Writer, value LaunchPhase) {
	switch variant_value := value.(type) {
	case LaunchPhaseBoot:
		writeInt32(writer, 1)
	case LaunchPhaseHydrating:
		writeInt32(writer, 2)
	case LaunchPhaseSilentChallenge:
		writeInt32(writer, 3)
		FfiConverterUint32INSTANCE.Write(writer, variant_value.Attempt)
	case LaunchPhaseRefreshing:
		writeInt32(writer, 4)
		FfiConverterRefreshReasonINSTANCE.Write(writer, variant_value.Reason)
	case LaunchPhaseOnline:
		writeInt32(writer, 5)
	case LaunchPhaseOffline:
		writeInt32(writer, 6)
		FfiConverterBoolINSTANCE.Write(writer, variant_value.Transient)
	case LaunchPhaseWizardAt:
		writeInt32(writer, 7)
		FfiConverterLaunchWizardEntryINSTANCE.Write(writer, variant_value.Entry)
	case LaunchPhaseIdentityChanged:
		writeInt32(writer, 8)
		FfiConverterStringINSTANCE.Write(writer, variant_value.PinnedHex)
		FfiConverterOptionalStringINSTANCE.Write(writer, variant_value.SeenHex)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterLaunchPhase.Write", value))
	}
}

type FfiDestroyerLaunchPhase struct{}

func (_ FfiDestroyerLaunchPhase) Destroy(value LaunchPhase) {
	value.Destroy()
}

// Entry points the launch flow's four-case branch can hand the wizard.
// Maps to the `OnboardingMachine`'s step enum on the client side.
type LaunchWizardEntry uint

const (
	// Long-term store has no identity. Wizard shows IdentityChoice.
	LaunchWizardEntryIdentityChoice LaunchWizardEntry = 1
	// Identity present, no nest_url, no pending invite. Wizard shows HandleEntry.
	LaunchWizardEntryHandleEntry LaunchWizardEntry = 2
	// Identity present, no nest_url, pending invite present. Wizard shows InviteRequest.
	LaunchWizardEntryInviteRequest LaunchWizardEntry = 3
	// Identity present + an awaiting-manual-dns slot: the user provisioned a
	// nest, chose "Set up later" for DNS, and quit. Checked **before** the
	// silent-challenge row — while DNS is pending the nest is unreachable by
	// definition, so challenging a saved `nest_url` would only fall through to
	// `launch_retry`.
	//
	// The client seeds `seed_identity(secret)` + `seed_awaiting_manual_dns(
	// nest_url, handle, dns_records, claim_code)` from the slot and renders the
	// "Almost ready" surface, which polls `recheck_manual_dns()` until the nest
	// resolves and the claim lands. This is *not* an `OnboardingStep` — the
	// surface is keyed on `wizard_outcome() == AwaitingManualDns`, so the
	// same-session exit and this relaunch-hydration path render identically.
	// See `docs/goal/behavior/onboarding.md` § App-launch routing +
	// § "Almost ready" surface.
	LaunchWizardEntryAwaitingManualDns LaunchWizardEntry = 4
	// Saved identity + saved nest URL where /verify returned 404 AND
	// `setup-status.claimed == false`. The nest is up but unclaimed,
	// so the user must claim it themselves before any registration is
	// possible. Wizard shows ClaimCode. See
	// `docs/goal/behavior/onboarding.md` § App-launch routing — silent-challenge
	// fallback table (unclaimed-nest row).
	LaunchWizardEntryClaimCode LaunchWizardEntry = 5
	// Identity present + a pending-factory-reset slot: the user factory-reset
	// their nest and the client died (or was quit) before the re-claim
	// completed. Checked **before** every other row — the box was wiped to
	// fresh/unclaimed, so a silent challenge against the saved `nest_url` would
	// only fall through to `launch_retry`, and the claim the user owes is the
	// one the slot pins.
	//
	// The distinction from [`Self::ClaimCode`] is the **pre-filled code**: the
	// client minted and pinned it before dispatching the reset (gap CR-1,
	// `docs/goal/architecture/nest/common.md` § Client-state recoverability), so
	// it can seed `navigate_to_claim_code_for_known_nest_with_code(nest_url,
	// handle, claim_code)` from the slot rather than asking the user for a code
	// that only ever existed in a reply their client never rendered.
	LaunchWizardEntryPendingFactoryReset LaunchWizardEntry = 6
)

type FfiConverterLaunchWizardEntry struct{}

var FfiConverterLaunchWizardEntryINSTANCE = FfiConverterLaunchWizardEntry{}

func (c FfiConverterLaunchWizardEntry) Lift(rb RustBufferI) LaunchWizardEntry {
	return LiftFromRustBuffer[LaunchWizardEntry](c, rb)
}

func (c FfiConverterLaunchWizardEntry) Lower(value LaunchWizardEntry) C.RustBuffer {
	return LowerIntoRustBuffer[LaunchWizardEntry](c, value)
}

func (c FfiConverterLaunchWizardEntry) LowerExternal(value LaunchWizardEntry) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[LaunchWizardEntry](c, value))
}
func (FfiConverterLaunchWizardEntry) Read(reader io.Reader) LaunchWizardEntry {
	id := readInt32(reader)
	return LaunchWizardEntry(id)
}

func (FfiConverterLaunchWizardEntry) Write(writer io.Writer, value LaunchWizardEntry) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerLaunchWizardEntry struct{}

func (_ FfiDestroyerLaunchWizardEntry) Destroy(value LaunchWizardEntry) {
}

// Why the LaunchMachine kicked off a token refresh. Observer-visible so
// telemetry can distinguish proactive refresh from reactive 401 handling.
type RefreshReason uint

const (
	// Pre-expiry refresh fired by the TTL scheduler.
	RefreshReasonScheduledTtl RefreshReason = 1
	// HTTP layer reported a 401; LaunchMachine is re-issuing the token.
	RefreshReasonGot401 RefreshReason = 2
	// Caller asked for a refresh explicitly.
	RefreshReasonManual RefreshReason = 3
)

type FfiConverterRefreshReason struct{}

var FfiConverterRefreshReasonINSTANCE = FfiConverterRefreshReason{}

func (c FfiConverterRefreshReason) Lift(rb RustBufferI) RefreshReason {
	return LiftFromRustBuffer[RefreshReason](c, rb)
}

func (c FfiConverterRefreshReason) Lower(value RefreshReason) C.RustBuffer {
	return LowerIntoRustBuffer[RefreshReason](c, value)
}

func (c FfiConverterRefreshReason) LowerExternal(value RefreshReason) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RefreshReason](c, value))
}
func (FfiConverterRefreshReason) Read(reader io.Reader) RefreshReason {
	id := readInt32(reader)
	return RefreshReason(id)
}

func (FfiConverterRefreshReason) Write(writer io.Writer, value RefreshReason) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerRefreshReason struct{}

func (_ FfiDestroyerRefreshReason) Destroy(value RefreshReason) {
}

// Where the bearer token lives in the lifecycle. Surfaced so HTTP layers
// can decide whether to wait for a refresh or proceed.
type TokenStatus interface {
	Destroy()
}

// No token present (initial state, or after sign-out).
type TokenStatusNone struct {
}

func (e TokenStatusNone) Destroy() {
}

// Token is valid. `expires_at_secs` is unix seconds **on this device's
// clock**: the nest's `expires_in` anchored to the launch clock at receipt
// (`fauna_protocol::auth::deadline_on_own_clock`), so every deadline the
// machine and its apps derive from it is compared against the clock it was
// made on. Only when the clock could not be read at receipt is it the
// nest's raw `expires_at`.
type TokenStatusValid struct {
	ExpiresAtSecs uint64
}

func (e TokenStatusValid) Destroy() {
	FfiDestroyerUint64{}.Destroy(e.ExpiresAtSecs)
}

// Token has passed its TTL but no refresh is in flight yet.
type TokenStatusExpired struct {
}

func (e TokenStatusExpired) Destroy() {
}

// A refresh is in flight; HTTP layers should wait.
type TokenStatusRefreshing struct {
}

func (e TokenStatusRefreshing) Destroy() {
}

type FfiConverterTokenStatus struct{}

var FfiConverterTokenStatusINSTANCE = FfiConverterTokenStatus{}

func (c FfiConverterTokenStatus) Lift(rb RustBufferI) TokenStatus {
	return LiftFromRustBuffer[TokenStatus](c, rb)
}

func (c FfiConverterTokenStatus) Lower(value TokenStatus) C.RustBuffer {
	return LowerIntoRustBuffer[TokenStatus](c, value)
}

func (c FfiConverterTokenStatus) LowerExternal(value TokenStatus) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[TokenStatus](c, value))
}
func (FfiConverterTokenStatus) Read(reader io.Reader) TokenStatus {
	id := readInt32(reader)
	switch id {
	case 1:
		return TokenStatusNone{}
	case 2:
		return TokenStatusValid{
			FfiConverterUint64INSTANCE.Read(reader),
		}
	case 3:
		return TokenStatusExpired{}
	case 4:
		return TokenStatusRefreshing{}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterTokenStatus.Read()", id))
	}
}

func (FfiConverterTokenStatus) Write(writer io.Writer, value TokenStatus) {
	switch variant_value := value.(type) {
	case TokenStatusNone:
		writeInt32(writer, 1)
	case TokenStatusValid:
		writeInt32(writer, 2)
		FfiConverterUint64INSTANCE.Write(writer, variant_value.ExpiresAtSecs)
	case TokenStatusExpired:
		writeInt32(writer, 3)
	case TokenStatusRefreshing:
		writeInt32(writer, 4)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterTokenStatus.Write", value))
	}
}

type FfiDestroyerTokenStatus struct{}

func (_ FfiDestroyerTokenStatus) Destroy(value TokenStatus) {
	value.Destroy()
}

type FfiConverterOptionalUint64 struct{}

var FfiConverterOptionalUint64INSTANCE = FfiConverterOptionalUint64{}

func (c FfiConverterOptionalUint64) Lift(rb RustBufferI) *uint64 {
	return LiftFromRustBuffer[*uint64](c, rb)
}

func (_ FfiConverterOptionalUint64) Read(reader io.Reader) *uint64 {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterUint64INSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalUint64) Lower(value *uint64) C.RustBuffer {
	return LowerIntoRustBuffer[*uint64](c, value)
}

func (c FfiConverterOptionalUint64) LowerExternal(value *uint64) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*uint64](c, value))
}

func (_ FfiConverterOptionalUint64) Write(writer io.Writer, value *uint64) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterUint64INSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalUint64 struct{}

func (_ FfiDestroyerOptionalUint64) Destroy(value *uint64) {
	if value != nil {
		FfiDestroyerUint64{}.Destroy(*value)
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

type FfiConverterOptionalAwaitingDnsRecord struct{}

var FfiConverterOptionalAwaitingDnsRecordINSTANCE = FfiConverterOptionalAwaitingDnsRecord{}

func (c FfiConverterOptionalAwaitingDnsRecord) Lift(rb RustBufferI) *AwaitingDnsRecord {
	return LiftFromRustBuffer[*AwaitingDnsRecord](c, rb)
}

func (_ FfiConverterOptionalAwaitingDnsRecord) Read(reader io.Reader) *AwaitingDnsRecord {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterAwaitingDnsRecordINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalAwaitingDnsRecord) Lower(value *AwaitingDnsRecord) C.RustBuffer {
	return LowerIntoRustBuffer[*AwaitingDnsRecord](c, value)
}

func (c FfiConverterOptionalAwaitingDnsRecord) LowerExternal(value *AwaitingDnsRecord) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*AwaitingDnsRecord](c, value))
}

func (_ FfiConverterOptionalAwaitingDnsRecord) Write(writer io.Writer, value *AwaitingDnsRecord) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterAwaitingDnsRecordINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalAwaitingDnsRecord struct{}

func (_ FfiDestroyerOptionalAwaitingDnsRecord) Destroy(value *AwaitingDnsRecord) {
	if value != nil {
		FfiDestroyerAwaitingDnsRecord{}.Destroy(*value)
	}
}

type FfiConverterOptionalLaunchIdentity struct{}

var FfiConverterOptionalLaunchIdentityINSTANCE = FfiConverterOptionalLaunchIdentity{}

func (c FfiConverterOptionalLaunchIdentity) Lift(rb RustBufferI) *LaunchIdentity {
	return LiftFromRustBuffer[*LaunchIdentity](c, rb)
}

func (_ FfiConverterOptionalLaunchIdentity) Read(reader io.Reader) *LaunchIdentity {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterLaunchIdentityINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalLaunchIdentity) Lower(value *LaunchIdentity) C.RustBuffer {
	return LowerIntoRustBuffer[*LaunchIdentity](c, value)
}

func (c FfiConverterOptionalLaunchIdentity) LowerExternal(value *LaunchIdentity) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*LaunchIdentity](c, value))
}

func (_ FfiConverterOptionalLaunchIdentity) Write(writer io.Writer, value *LaunchIdentity) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterLaunchIdentityINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalLaunchIdentity struct{}

func (_ FfiDestroyerOptionalLaunchIdentity) Destroy(value *LaunchIdentity) {
	if value != nil {
		FfiDestroyerLaunchIdentity{}.Destroy(*value)
	}
}

type FfiConverterOptionalPendingFactoryResetRecord struct{}

var FfiConverterOptionalPendingFactoryResetRecordINSTANCE = FfiConverterOptionalPendingFactoryResetRecord{}

func (c FfiConverterOptionalPendingFactoryResetRecord) Lift(rb RustBufferI) *PendingFactoryResetRecord {
	return LiftFromRustBuffer[*PendingFactoryResetRecord](c, rb)
}

func (_ FfiConverterOptionalPendingFactoryResetRecord) Read(reader io.Reader) *PendingFactoryResetRecord {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterPendingFactoryResetRecordINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalPendingFactoryResetRecord) Lower(value *PendingFactoryResetRecord) C.RustBuffer {
	return LowerIntoRustBuffer[*PendingFactoryResetRecord](c, value)
}

func (c FfiConverterOptionalPendingFactoryResetRecord) LowerExternal(value *PendingFactoryResetRecord) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*PendingFactoryResetRecord](c, value))
}

func (_ FfiConverterOptionalPendingFactoryResetRecord) Write(writer io.Writer, value *PendingFactoryResetRecord) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterPendingFactoryResetRecordINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalPendingFactoryResetRecord struct{}

func (_ FfiDestroyerOptionalPendingFactoryResetRecord) Destroy(value *PendingFactoryResetRecord) {
	if value != nil {
		FfiDestroyerPendingFactoryResetRecord{}.Destroy(*value)
	}
}

type FfiConverterOptionalPendingInviteRecord struct{}

var FfiConverterOptionalPendingInviteRecordINSTANCE = FfiConverterOptionalPendingInviteRecord{}

func (c FfiConverterOptionalPendingInviteRecord) Lift(rb RustBufferI) *PendingInviteRecord {
	return LiftFromRustBuffer[*PendingInviteRecord](c, rb)
}

func (_ FfiConverterOptionalPendingInviteRecord) Read(reader io.Reader) *PendingInviteRecord {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterPendingInviteRecordINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalPendingInviteRecord) Lower(value *PendingInviteRecord) C.RustBuffer {
	return LowerIntoRustBuffer[*PendingInviteRecord](c, value)
}

func (c FfiConverterOptionalPendingInviteRecord) LowerExternal(value *PendingInviteRecord) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*PendingInviteRecord](c, value))
}

func (_ FfiConverterOptionalPendingInviteRecord) Write(writer io.Writer, value *PendingInviteRecord) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterPendingInviteRecordINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalPendingInviteRecord struct{}

func (_ FfiDestroyerOptionalPendingInviteRecord) Destroy(value *PendingInviteRecord) {
	if value != nil {
		FfiDestroyerPendingInviteRecord{}.Destroy(*value)
	}
}

type FfiConverterOptionalAccountIndexRefusal struct{}

var FfiConverterOptionalAccountIndexRefusalINSTANCE = FfiConverterOptionalAccountIndexRefusal{}

func (c FfiConverterOptionalAccountIndexRefusal) Lift(rb RustBufferI) *AccountIndexRefusal {
	return LiftFromRustBuffer[*AccountIndexRefusal](c, rb)
}

func (_ FfiConverterOptionalAccountIndexRefusal) Read(reader io.Reader) *AccountIndexRefusal {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterAccountIndexRefusalINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalAccountIndexRefusal) Lower(value *AccountIndexRefusal) C.RustBuffer {
	return LowerIntoRustBuffer[*AccountIndexRefusal](c, value)
}

func (c FfiConverterOptionalAccountIndexRefusal) LowerExternal(value *AccountIndexRefusal) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*AccountIndexRefusal](c, value))
}

func (_ FfiConverterOptionalAccountIndexRefusal) Write(writer io.Writer, value *AccountIndexRefusal) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterAccountIndexRefusalINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalAccountIndexRefusal struct{}

func (_ FfiDestroyerOptionalAccountIndexRefusal) Destroy(value *AccountIndexRefusal) {
	if value != nil {
		FfiDestroyerAccountIndexRefusal{}.Destroy(*value)
	}
}

const (
	uniffiRustFuturePollReady      int8 = 0
	uniffiRustFuturePollMaybeReady int8 = 1
)

type rustFuturePollFunc func(C.uint64_t, C.UniffiRustFutureContinuationCallback, C.uint64_t)
type rustFutureCompleteFunc[T any] func(C.uint64_t, *C.RustCallStatus) T
type rustFutureFreeFunc func(C.uint64_t)

//export fauna_launch_machine_uniffiFutureContinuationCallback
func fauna_launch_machine_uniffiFutureContinuationCallback(data C.uint64_t, pollResult C.int8_t) {
	h := cgo.Handle(uintptr(data))
	waiter := h.Value().(chan int8)
	waiter <- int8(pollResult)
}

func uniffiRustCallAsync[E any, T any, F any](
	errConverter BufReader[E],
	completeFunc rustFutureCompleteFunc[F],
	liftFunc func(F) T,
	rustFuture C.uint64_t,
	pollFunc rustFuturePollFunc,
	freeFunc rustFutureFreeFunc,
) (T, E) {
	defer freeFunc(rustFuture)

	pollResult := int8(-1)
	waiter := make(chan int8, 1)

	chanHandle := cgo.NewHandle(waiter)
	defer chanHandle.Delete()

	for pollResult != uniffiRustFuturePollReady {
		pollFunc(
			rustFuture,
			(C.UniffiRustFutureContinuationCallback)(C.fauna_launch_machine_uniffiFutureContinuationCallback),
			C.uint64_t(chanHandle),
		)
		pollResult = <-waiter
	}

	var goValue T
	ffiValue, err := rustCallWithError(errConverter, func(status *C.RustCallStatus) F {
		return completeFunc(rustFuture, status)
	})
	if value := reflect.ValueOf(err); value.IsValid() && !value.IsZero() {
		return goValue, err
	}
	return liftFunc(ffiValue), err
}

//export fauna_launch_machine_uniffiFreeGorutine
func fauna_launch_machine_uniffiFreeGorutine(data C.uint64_t) {
	handle := cgo.Handle(uintptr(data))
	defer handle.Delete()

	guard := handle.Value().(chan struct{})
	guard <- struct{}{}
}

// Production twin: a shipped build has no override state and no way to install
// one, so the dial URL and the identity URL are the same string.
//
// The `cfg`-split lives here — once, in the crate every app's launch path
// already runs through — rather than as seven per-app wrappers, so no app can
// grow its own dialect of the rule while a release artifact still carries
// neither the override state nor its setter.
func ResolvedDialUrl(nestUrl string) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_launch_machine_fn_func_resolved_dial_url(FfiConverterStringINSTANCE.Lower(nestUrl), _uniffiStatus),
		}
	}))
}

// Mint the post-reset claim code and durably persist it **before** the caller
// dispatches `fauna.admin.factory_reset` — the single atomic decision point
// that closes gap CR-1 (`docs/goal/architecture/nest/common.md`
// § Client-state recoverability).
//
// Returning the code only *after* the store write is what makes the crash-unsafe
// ordering unrepresentable: a caller cannot hold a code it has not already
// persisted. Pin the returned code via `AdminClient::factory_reset(Some(code))`;
// if the client dies anywhere after this call, the relaunch finds the slot and
// resumes the claim ([`LaunchWizardEntry::PendingFactoryReset`]).
//
// The code format is `fauna_core::claim_code` (8 chars / 40 bits, grouped),
// shared with the nest's own minting path, so a client-minted and a nest-minted
// code are byte-identical.
//
// [`LaunchWizardEntry::PendingFactoryReset`]: crate::LaunchWizardEntry::PendingFactoryReset
func MintAndPersistPendingFactoryReset(store LaunchPersistence, nestUrl string, handle string) *string {
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_launch_machine_fn_func_mint_and_persist_pending_factory_reset(FfiConverterLaunchPersistenceINSTANCE.Lower(store), FfiConverterStringINSTANCE.Lower(nestUrl), FfiConverterStringINSTANCE.Lower(handle), _uniffiStatus),
		}
	}))
}
