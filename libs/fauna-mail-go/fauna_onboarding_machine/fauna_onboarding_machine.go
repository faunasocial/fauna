package fauna_onboarding_machine

// #include <fauna_onboarding_machine.h>
import "C"

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"
	"github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_launch_machine"
	"github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_provisioning"
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
		C.ffi_fauna_onboarding_machine_rustbuffer_free(cb.inner, status)
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
		return C.ffi_fauna_onboarding_machine_rustbuffer_from_bytes(foreign, status)
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

	FfiConverterOnboardingObserverINSTANCE.register()
	uniffiCheckChecksums()
}

func uniffiCheckChecksums() {
	// Get the bindings contract version from our ComponentInterface
	bindingsContractVersion := 30
	// Get the scaffolding contract version by calling the into the dylib
	scaffoldingContractVersion := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint32_t {
		return C.ffi_fauna_onboarding_machine_uniffi_contract_version()
	})
	if bindingsContractVersion != int(scaffoldingContractVersion) {
		// If this happens try cleaning and rebuilding your project
		panic("fauna_onboarding_machine: UniFFI contract version mismatch")
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_func_format_price()
		})
		if checksum != 63858 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_func_format_price: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_func_handle_tld()
		})
		if checksum != 37584 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_func_handle_tld: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_func_qualify_reclaim_handle()
		})
		if checksum != 46450 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_func_qualify_reclaim_handle: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_func_server_type_allowed_for_mail()
		})
		if checksum != 2899 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_func_server_type_allowed_for_mail: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_func_server_type_label()
		})
		if checksum != 29811 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_func_server_type_label: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_func_recovery_entry_outcome_message()
		})
		if checksum != 57126 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_func_recovery_entry_outcome_message: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_func_awaiting_dns_poll_ms()
		})
		if checksum != 59588 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_func_awaiting_dns_poll_ms: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_func_invite_recheck_poll_ms()
		})
		if checksum != 40438 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_func_invite_recheck_poll_ms: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_adminnatmodemachine_hydrate()
		})
		if checksum != 35286 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_adminnatmodemachine_hydrate: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_adminnatmodemachine_select()
		})
		if checksum != 30138 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_adminnatmodemachine_select: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_adminnatmodemachine_snapshot()
		})
		if checksum != 28572 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_adminnatmodemachine_snapshot: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_adminnatmodemachine_submit()
		})
		if checksum != 17623 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_adminnatmodemachine_submit: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_abandon_awaiting_manual_dns()
		})
		if checksum != 42438 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_abandon_awaiting_manual_dns: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_age_claim()
		})
		if checksum != 60097 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_age_claim: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_age_claim_digest()
		})
		if checksum != 24807 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_age_claim_digest: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_age_claim_message()
		})
		if checksum != 20781 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_age_claim_message: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_awaiting_dns_copy_enabled()
		})
		if checksum != 56212 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_awaiting_dns_copy_enabled: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_awaiting_dns_fallthrough_enabled()
		})
		if checksum != 27536 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_awaiting_dns_fallthrough_enabled: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_awaiting_dns_records_json()
		})
		if checksum != 46973 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_awaiting_dns_records_json: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_awaiting_dns_records_text()
		})
		if checksum != 64418 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_awaiting_dns_records_text: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_awaiting_manual_dns_snapshot()
		})
		if checksum != 58504 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_awaiting_manual_dns_snapshot: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_back()
		})
		if checksum != 48648 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_back: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_begin_create_identity()
		})
		if checksum != 62104 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_begin_create_identity: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_begin_import_identity()
		})
		if checksum != 37068 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_begin_import_identity: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_begin_import_identity_with_reason()
		})
		if checksum != 27257 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_begin_import_identity_with_reason: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_begin_recover_lost_box()
		})
		if checksum != 36676 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_begin_recover_lost_box: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_begin_recovery_entry()
		})
		if checksum != 18541 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_begin_recovery_entry: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_bill_of_materials()
		})
		if checksum != 45320 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_bill_of_materials: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_bom_domain_line()
		})
		if checksum != 41746 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_bom_domain_line: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_bom_vps_line()
		})
		if checksum != 27033 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_bom_vps_line: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_caldav_enable_requested()
		})
		if checksum != 46553 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_caldav_enable_requested: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_can_continue_dns()
		})
		if checksum != 43232 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_can_continue_dns: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_can_continue_provisioning()
		})
		if checksum != 44768 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_can_continue_provisioning: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_can_continue_vps()
		})
		if checksum != 46642 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_can_continue_vps: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_can_retry_provisioning()
		})
		if checksum != 21631 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_can_retry_provisioning: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_can_verify_dns()
		})
		if checksum != 36120 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_can_verify_dns: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_can_verify_vps()
		})
		if checksum != 30105 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_can_verify_vps: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_cancel_handle_check()
		})
		if checksum != 39513 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_cancel_handle_check: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_cancel_invite_op()
		})
		if checksum != 7139 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_cancel_invite_op: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_cancel_provisioning()
		})
		if checksum != 62211 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_cancel_provisioning: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_captured_dns_credential()
		})
		if checksum != 10359 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_captured_dns_credential: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_carddav_enable_requested()
		})
		if checksum != 5677 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_carddav_enable_requested: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_claim_code_prefill()
		})
		if checksum != 56595 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_claim_code_prefill: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_claim_code_snapshot()
		})
		if checksum != 42302 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_claim_code_snapshot: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_clear_error()
		})
		if checksum != 37083 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_clear_error: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_complete()
		})
		if checksum != 46798 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_complete: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_complete_probe_error()
		})
		if checksum != 12358 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_complete_probe_error: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_confirm_generated_identity()
		})
		if checksum != 28278 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_confirm_generated_identity: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_confirm_imported_identity()
		})
		if checksum != 35328 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_confirm_imported_identity: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_confirm_price()
		})
		if checksum != 7644 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_confirm_price: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_confirm_recovery_kit()
		})
		if checksum != 30061 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_confirm_recovery_kit: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_continue_from_dns()
		})
		if checksum != 19857 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_continue_from_dns: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_continue_from_dns_post_instructions()
		})
		if checksum != 1390 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_continue_from_dns_post_instructions: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_continue_from_provisioning()
		})
		if checksum != 10657 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_continue_from_provisioning: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_continue_from_vps()
		})
		if checksum != 39501 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_continue_from_vps: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_current_handle()
		})
		if checksum != 62370 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_current_handle: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_defer_nat_mode_choice()
		})
		if checksum != 46496 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_defer_nat_mode_choice: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_dns_config()
		})
		if checksum != 3893 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_dns_config: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_dns_post_instructions()
		})
		if checksum != 334 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_dns_post_instructions: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_dns_provider_eligible()
		})
		if checksum != 2110 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_dns_provider_eligible: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_dns_provider_ineligible_reason()
		})
		if checksum != 21037 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_dns_provider_ineligible_reason: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_dns_records()
		})
		if checksum != 7335 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_dns_records: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_dns_set_up_later()
		})
		if checksum != 53547 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_dns_set_up_later: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_dns_status_text_key()
		})
		if checksum != 4025 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_dns_status_text_key: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_domain_status()
		})
		if checksum != 15197 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_domain_status: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_effective_nest_url()
		})
		if checksum != 26947 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_effective_nest_url: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_effective_secret()
		})
		if checksum != 12222 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_effective_secret: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_email_enable_requested()
		})
		if checksum != 21585 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_email_enable_requested: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_error_message()
		})
		if checksum != 39987 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_error_message: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_fail_recovery_entry()
		})
		if checksum != 60724 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_fail_recovery_entry: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_generated_secret()
		})
		if checksum != 12188 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_generated_secret: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_grant_default_trust()
		})
		if checksum != 59357 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_grant_default_trust: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_handle_check_snapshot()
		})
		if checksum != 56476 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_handle_check_snapshot: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_hosted_auth_begin()
		})
		if checksum != 38817 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_hosted_auth_begin: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_hosted_auth_button_text()
		})
		if checksum != 46159 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_hosted_auth_button_text: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_hosted_auth_can_begin()
		})
		if checksum != 34442 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_hosted_auth_can_begin: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_hosted_auth_state()
		})
		if checksum != 60689 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_hosted_auth_state: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_hosted_auth_wait()
		})
		if checksum != 15621 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_hosted_auth_wait: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_identity_origin()
		})
		if checksum != 20775 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_identity_origin: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_invite_error()
		})
		if checksum != 52939 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_invite_error: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_invite_request_snapshot()
		})
		if checksum != 21646 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_invite_request_snapshot: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_is_buyable_via_provider()
		})
		if checksum != 40560 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_is_buyable_via_provider: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_is_loading()
		})
		if checksum != 30275 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_is_loading: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_local_nest_reachable()
		})
		if checksum != 43325 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_local_nest_reachable: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_nat_mode_snapshot()
		})
		if checksum != 26584 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_nat_mode_snapshot: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_navigate_to_claim_code_for_known_nest()
		})
		if checksum != 19910 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_navigate_to_claim_code_for_known_nest: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_navigate_to_claim_code_for_known_nest_with_code()
		})
		if checksum != 42895 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_navigate_to_claim_code_for_known_nest_with_code: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_navigate_to_invite_request_for_known_nest()
		})
		if checksum != 8277 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_navigate_to_invite_request_for_known_nest: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_nest_url()
		})
		if checksum != 4797 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_nest_url: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_pending_invite_slot()
		})
		if checksum != 45110 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_pending_invite_slot: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_pending_invite_status_json()
		})
		if checksum != 39956 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_pending_invite_status_json: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_probe_setup_status_at()
		})
		if checksum != 23174 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_probe_setup_status_at: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_provider_status()
		})
		if checksum != 10418 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_provider_status: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_provision_mail_mode_enabled()
		})
		if checksum != 5834 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_provision_mail_mode_enabled: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_provision_reach_ipv4()
		})
		if checksum != 42734 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_provision_reach_ipv4: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_provision_update_channel()
		})
		if checksum != 47483 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_provision_update_channel: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_provisioning_continue_blocked_reason()
		})
		if checksum != 52607 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_provisioning_continue_blocked_reason: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_provisioning_in_progress()
		})
		if checksum != 21474 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_provisioning_in_progress: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_provisioning_snapshot()
		})
		if checksum != 15252 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_provisioning_snapshot: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recheck_invite_status()
		})
		if checksum != 26812 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recheck_invite_status: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recheck_manual_dns()
		})
		if checksum != 28497 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recheck_manual_dns: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recover_via_cloud()
		})
		if checksum != 3427 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recover_via_cloud: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recover_via_selfhosted()
		})
		if checksum != 39888 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recover_via_selfhosted: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recovery_boxes()
		})
		if checksum != 25912 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recovery_boxes: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recovery_came_from()
		})
		if checksum != 27750 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recovery_came_from: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recovery_intent()
		})
		if checksum != 49408 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recovery_intent: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recovery_kit_secret_hex()
		})
		if checksum != 16449 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recovery_kit_secret_hex: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recovery_kit_uri()
		})
		if checksum != 16532 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recovery_kit_uri: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recovery_selected_nest_id()
		})
		if checksum != 55615 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_recovery_selected_nest_id: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_redeem_invite()
		})
		if checksum != 50369 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_redeem_invite: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_request_age_nonce()
		})
		if checksum != 27875 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_request_age_nonce: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_reset()
		})
		if checksum != 38354 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_reset: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_resolve_not_found_recheck()
		})
		if checksum != 20711 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_resolve_not_found_recheck: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_resolve_restore_target()
		})
		if checksum != 48603 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_resolve_restore_target: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_restored_predecessors()
		})
		if checksum != 22425 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_restored_predecessors: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_restored_predecessors_unreadable()
		})
		if checksum != 34420 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_restored_predecessors_unreadable: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_retry_provisioning()
		})
		if checksum != 55525 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_retry_provisioning: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_run_handle_check_phases()
		})
		if checksum != 63462 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_run_handle_check_phases: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_run_provisioning()
		})
		if checksum != 20541 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_run_provisioning: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_seed_at_invite_request_unregistered()
		})
		if checksum != 55347 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_seed_at_invite_request_unregistered: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_seed_awaiting_manual_dns()
		})
		if checksum != 8710 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_seed_awaiting_manual_dns: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_seed_awaiting_manual_dns_json()
		})
		if checksum != 599 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_seed_awaiting_manual_dns_json: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_seed_awaiting_manual_dns_record()
		})
		if checksum != 61536 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_seed_awaiting_manual_dns_record: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_seed_identity()
		})
		if checksum != 9250 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_seed_identity: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_seed_identity_for_recovery()
		})
		if checksum != 16471 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_seed_identity_for_recovery: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_seed_pending_invite()
		})
		if checksum != 62429 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_seed_pending_invite: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_select_dns_provider()
		})
		if checksum != 14363 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_select_dns_provider: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_select_nat_mode()
		})
		if checksum != 15264 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_select_nat_mode: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_select_recovery_box()
		})
		if checksum != 17657 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_select_recovery_box: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_select_vps_location()
		})
		if checksum != 15755 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_select_vps_location: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_select_vps_provider()
		})
		if checksum != 62486 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_select_vps_provider: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_select_vps_server_type()
		})
		if checksum != 43887 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_select_vps_server_type: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_selected_registrar_requires_contact()
		})
		if checksum != 16749 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_selected_registrar_requires_contact: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_age_claim()
		})
		if checksum != 30166 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_age_claim: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_contact()
		})
		if checksum != 47433 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_contact: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_control_checkbox()
		})
		if checksum != 59338 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_control_checkbox: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_current_handle()
		})
		if checksum != 7655 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_current_handle: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_dns_cred()
		})
		if checksum != 51461 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_dns_cred: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_error_message()
		})
		if checksum != 8884 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_error_message: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_nest_hint()
		})
		if checksum != 2828 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_nest_hint: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_nest_url()
		})
		if checksum != 14316 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_nest_url: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_phase()
		})
		if checksum != 31188 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_phase: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_provision_mail_mode()
		})
		if checksum != 2276 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_provision_mail_mode: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_provision_update_channel()
		})
		if checksum != 39230 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_provision_update_channel: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_recovery_boxes()
		})
		if checksum != 35376 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_recovery_boxes: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_renders_recovery_kit()
		})
		if checksum != 46752 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_renders_recovery_kit: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_renders_trust_prompt()
		})
		if checksum != 18399 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_renders_trust_prompt: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_store_container_dir()
		})
		if checksum != 4475 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_store_container_dir: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_vps_cred()
		})
		if checksum != 31818 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_set_vps_cred: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_should_show_contact_form()
		})
		if checksum != 59231 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_should_show_contact_form: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_should_show_no_provider_message()
		})
		if checksum != 17334 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_should_show_no_provider_message: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_should_show_registrar_notes()
		})
		if checksum != 29159 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_should_show_registrar_notes: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_skip_recovery_kit()
		})
		if checksum != 51942 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_skip_recovery_kit: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_skip_trust_prompt()
		})
		if checksum != 20794 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_skip_trust_prompt: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_start_handle_check()
		})
		if checksum != 38331 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_start_handle_check: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_start_provisioning()
		})
		if checksum != 4803 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_start_provisioning: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_step()
		})
		if checksum != 4854 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_step: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_submit_handle_check_continue()
		})
		if checksum != 4462 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_submit_handle_check_continue: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_submit_nat_mode_choice()
		})
		if checksum != 58335 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_submit_nat_mode_choice: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_submit_recovery_entry()
		})
		if checksum != 9836 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_submit_recovery_entry: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_take_pending_recovery_secret()
		})
		if checksum != 18010 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_take_pending_recovery_secret: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_take_trust_prompt_granted()
		})
		if checksum != 6041 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_take_trust_prompt_granted: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_toggle_buy_domain()
		})
		if checksum != 65207 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_toggle_buy_domain: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_toggle_same_provider_for_vps()
		})
		if checksum != 20038 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_toggle_same_provider_for_vps: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_verify_dns()
		})
		if checksum != 54661 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_verify_dns: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_verify_oob_invite_code()
		})
		if checksum != 46782 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_verify_oob_invite_code: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_verify_vps()
		})
		if checksum != 58159 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_verify_vps: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_visible_dns_fields()
		})
		if checksum != 27486 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_visible_dns_fields: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_visible_vps_fields()
		})
		if checksum != 10250 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_visible_vps_fields: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_vps_config()
		})
		if checksum != 42672 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_vps_config: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_vps_continue_blocked_reason()
		})
		if checksum != 49406 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_vps_continue_blocked_reason: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_webdav_enable_requested()
		})
		if checksum != 38207 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_webdav_enable_requested: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_wizard_outcome()
		})
		if checksum != 13868 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_wizard_outcome: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_wizard_submit_claim_code()
		})
		if checksum != 36960 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_wizard_submit_claim_code: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_wizard_submit_invite_request()
		})
		if checksum != 41033 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingmachine_wizard_submit_invite_request: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_onboardingobserver_on_changed()
		})
		if checksum != 10794 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_onboardingobserver_on_changed: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_add_candidate_domains()
		})
		if checksum != 54754 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_add_candidate_domains: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_app_inputs_landed()
		})
		if checksum != 23454 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_app_inputs_landed: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_back()
		})
		if checksum != 50193 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_back: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_begin_confirm()
		})
		if checksum != 1926 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_begin_confirm: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_can_verify()
		})
		if checksum != 38926 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_can_verify: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_cancel()
		})
		if checksum != 40497 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_cancel: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_confirm()
		})
		if checksum != 57263 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_confirm: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_confirm_summary()
		})
		if checksum != 63503 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_confirm_summary: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_expect_app_inputs()
		})
		if checksum != 54448 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_expect_app_inputs: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_fetch_transfer_code()
		})
		if checksum != 2444 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_fetch_transfer_code: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_force_server()
		})
		if checksum != 39971 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_force_server: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_hosted_auth_begin()
		})
		if checksum != 55957 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_hosted_auth_begin: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_hosted_auth_button_text()
		})
		if checksum != 26866 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_hosted_auth_button_text: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_hosted_auth_can_begin()
		})
		if checksum != 62084 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_hosted_auth_can_begin: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_hosted_auth_state()
		})
		if checksum != 22285 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_hosted_auth_state: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_hosted_auth_wait()
		})
		if checksum != 8589 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_hosted_auth_wait: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_leftover_lines()
		})
		if checksum != 20079 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_leftover_lines: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_retry()
		})
		if checksum != 52310 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_retry: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_select()
		})
		if checksum != 3505 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_select: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_select_provider()
		})
		if checksum != 55252 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_select_provider: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_selected_provider()
		})
		if checksum != 46353 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_selected_provider: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_set_confirm_name()
		})
		if checksum != 28178 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_set_confirm_name: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_set_credential_field()
		})
		if checksum != 12180 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_set_credential_field: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_set_current_ipv4()
		})
		if checksum != 36278 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_set_current_ipv4: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_set_held_dns_json()
		})
		if checksum != 38552 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_set_held_dns_json: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_snapshot()
		})
		if checksum != 6352 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_snapshot: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_step_label()
		})
		if checksum != 28134 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_step_label: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_step_note()
		})
		if checksum != 56706 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_step_note: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_transfer_code_note()
		})
		if checksum != 43583 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_transfer_code_note: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_verify()
		})
		if checksum != 1371 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_verify: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_visible_fields()
		})
		if checksum != 27980 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_method_nestretiremachine_visible_fields: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_constructor_adminnatmodemachine_new()
		})
		if checksum != 9969 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_constructor_adminnatmodemachine_new: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_constructor_onboardingmachine_new()
		})
		if checksum != 42950 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_constructor_onboardingmachine_new: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_constructor_onboardingmachine_new_with_persistence()
		})
		if checksum != 57940 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_constructor_onboardingmachine_new_with_persistence: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_onboarding_machine_checksum_constructor_nestretiremachine_new()
		})
		if checksum != 46407 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_onboarding_machine: uniffi_fauna_onboarding_machine_checksum_constructor_nestretiremachine_new: UniFFI API checksum mismatch")
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

// One instance per admin-nest page visit. Holds the rendered
// [`NatModeSnapshot`]; drives the shared [`NestApi`] seam. Dispatch-style
// like the admin policy machines: the view awaits each action and re-reads
// [`Self::snapshot`] — no observer.
type AdminNatModeMachineInterface interface {
	// Page load: read `fauna.setup.status` and pre-select the current
	// `node_mode` (an unknown spelling ⇒ `public`). A read failure surfaces as a
	// transient error but leaves save enabled — the set is safe to submit
	// without a successful read (mutable upsert).
	Hydrate()
	// Radio click (`admin-nest-nat-mode-{public,private}-radio`). Recovers
	// from `Error`/`Done` back to `Choosing`; save stays enabled.
	Select(mode fauna_core.NodeMode)
	// The rendered state. Pure read.
	Snapshot() NatModeSnapshot
	// Save (`admin-nest-nat-mode-save-button`): sign the canonical payload
	// and commit the selected mode via the mutable `fauna.setup.nat_mode`.
	// Success lands `Done` with save **re-enabled** (the admin may flip
	// again); failures land `Error` with save enabled (resubmit is always
	// allowed).
	Submit()
}

// One instance per admin-nest page visit. Holds the rendered
// [`NatModeSnapshot`]; drives the shared [`NestApi`] seam. Dispatch-style
// like the admin policy machines: the view awaits each action and re-reads
// [`Self::snapshot`] — no observer.
type AdminNatModeMachine struct {
	ffiObject FfiObject
}

// Production constructor: the pre-identity WS-RPC transport against
// `nest_url`, signing with the admin's identity `secret_hex` (the same
// secret the client authenticates with — the payload signature is the
// authorization, no bearer involved).
func NewAdminNatModeMachine(nestUrl string, secretHex fauna_core.SecretString) *AdminNatModeMachine {
	return FfiConverterAdminNatModeMachineINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_fauna_onboarding_machine_fn_constructor_adminnatmodemachine_new(FfiConverterStringINSTANCE.Lower(nestUrl),
			CFromRustBuffer(fauna_core.LowerToExternalTypeSecretString(secretHex)), _uniffiStatus)
	}))
}

// Page load: read `fauna.setup.status` and pre-select the current
// `node_mode` (an unknown spelling ⇒ `public`). A read failure surfaces as a
// transient error but leaves save enabled — the set is safe to submit
// without a successful read (mutable upsert).
func (_self *AdminNatModeMachine) Hydrate() {
	_pointer := _self.ffiObject.incrementPointer("*AdminNatModeMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_adminnatmodemachine_hydrate(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

}

// Radio click (`admin-nest-nat-mode-{public,private}-radio`). Recovers
// from `Error`/`Done` back to `Choosing`; save stays enabled.
func (_self *AdminNatModeMachine) Select(mode fauna_core.NodeMode) {
	_pointer := _self.ffiObject.incrementPointer("*AdminNatModeMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_adminnatmodemachine_select(
			_pointer,
			CFromRustBuffer(fauna_core.FfiConverterNodeModeINSTANCE.LowerExternal(mode)), _uniffiStatus)
		return false
	})
}

// The rendered state. Pure read.
func (_self *AdminNatModeMachine) Snapshot() NatModeSnapshot {
	_pointer := _self.ffiObject.incrementPointer("*AdminNatModeMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterNatModeSnapshotINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_adminnatmodemachine_snapshot(
				_pointer, _uniffiStatus),
		}
	}))
}

// Save (`admin-nest-nat-mode-save-button`): sign the canonical payload
// and commit the selected mode via the mutable `fauna.setup.nat_mode`.
// Success lands `Done` with save **re-enabled** (the admin may flip
// again); failures land `Error` with save enabled (resubmit is always
// allowed).
func (_self *AdminNatModeMachine) Submit() {
	_pointer := _self.ffiObject.incrementPointer("*AdminNatModeMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_adminnatmodemachine_submit(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

}
func (object *AdminNatModeMachine) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterAdminNatModeMachine struct{}

var FfiConverterAdminNatModeMachineINSTANCE = FfiConverterAdminNatModeMachine{}

func (c FfiConverterAdminNatModeMachine) Lift(handle C.uint64_t) *AdminNatModeMachine {
	result := &AdminNatModeMachine{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_fauna_onboarding_machine_fn_clone_adminnatmodemachine(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_fauna_onboarding_machine_fn_free_adminnatmodemachine(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*AdminNatModeMachine).Destroy)
	return result
}

func (c FfiConverterAdminNatModeMachine) Read(reader io.Reader) *AdminNatModeMachine {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterAdminNatModeMachine) Lower(value *AdminNatModeMachine) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*AdminNatModeMachine")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterAdminNatModeMachine) Write(writer io.Writer, value *AdminNatModeMachine) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalAdminNatModeMachine(handle uint64) *AdminNatModeMachine {
	return FfiConverterAdminNatModeMachineINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalAdminNatModeMachine(value *AdminNatModeMachine) uint64 {
	return uint64(FfiConverterAdminNatModeMachineINSTANCE.Lower(value))
}

type FfiDestroyerAdminNatModeMachine struct{}

func (_ FfiDestroyerAdminNatModeMachine) Destroy(value *AdminNatModeMachine) {
	value.Destroy()
}

// One instance per `nest_retire` page visit.
type NestRetireMachineInterface interface {
	// Add attribution candidates the app learned after the page opened — the
	// admin entry's active local-domain list (§ DNS cleanup → *Several
	// domains* (1)). Read at `verify`; a domain already held is not repeated.
	AddCandidateDomains(domains []string)
	// The announced inputs have landed — or their read failed, which keeps
	// what the page opened with. Verify is pressable again.
	AppInputsLanded()
	// Back from a later phase to the one before it. From `List` this returns
	// to the credential form; from `Confirm`, to the list. Either move
	// disarms; a run in flight or failed is left by `cancel`, not by `back`.
	Back()
	// Move to the confirm state, computing the DNS plan the summary shows.
	BeginConfirm()
	// Whether the verify button is pressable: a provider is picked, every
	// required field is filled in, no listing is in flight, and no input the
	// app announced is still on its way.
	CanVerify() bool
	// Back out of the page. **Drops the token** and every fetched code — the
	// credential stance is that neither outlives the visit.
	Cancel()
	// Run the retire: **DNS first, then the server.**
	//
	// Refuses unless the typed-name gate is satisfied. A DNS-step failure
	// stops the run *before the server is touched* and offers retry or
	// *delete the server anyway* — never a silent continue, because
	// continuing is what strands `A` records on an address about to be
	// reassigned.
	//
	// Passing the gate **arms** the run to the selected server: `retry` and
	// `force_server` act on that id and no other, so the typed name guards
	// every door into `delete_server`, not only this one.
	Confirm()
	// The confirm summary (§ Confirm shape), one sentence per line, in the
	// order the view shows them: what is deleted and where; the attributed
	// domain and every other verified domain; that the data cannot be
	// recovered except from a backup; on the current box, that this session's
	// nest stops existing; and the DNS plan — the records removed first, or
	// that none will be — followed by what is left to remove by hand.
	//
	// The `provider` argument of the first line is the provider's display
	// **key**, so render it with nested resolution.
	ConfirmSummary() []fauna_core.LocalizedText
	// The app is reading more inputs after the page opened (the admin
	// entry's live custody read and local-domain list): hold verify until
	// [`Self::app_inputs_landed`], because `verify` is where the credentials
	// and candidates are read, and a verify ahead of them would clean less
	// than the person's own config allows.
	ExpectAppInputs()
	// Fetch the selected row's transfer authorization code. Both arms are
	// rendered: the code, or the instant a registry lock lifts.
	FetchTransferCode()
	// Delete the server despite a failed DNS step (§ DNS cleanup: the failure
	// branch offers *retry* or *delete the server anyway*). Acts only on the
	// armed server, and only while the offer stands. The not-removed plan
	// lines join the by-hand list; the dangling-record warning is the view's
	// to repeat.
	ForceServer()
	// Step 1 of a `hosted-auth` field's sign-in: POST the device-authorization
	// request to the form's `base-url`. `Some` carries what the app opens and
	// shows; `None` means the attempt failed and the field's state says why.
	// Follow with [`Self::hosted_auth_wait`].
	HostedAuthBegin(fieldId string) *HostedAuthPrompt
	// The `hosted-auth` button's label — the wizard's one mapping.
	HostedAuthButtonText(fieldId string) string
	// Whether the sign-in button is pressable: the form's `base-url` is
	// filled in and no attempt on this field is mid-flight.
	HostedAuthCanBegin(fieldId string) bool
	// Where a `hosted-auth` credential field's sign-in stands — `Idle` for a
	// field never started. The retire view reuses `vps_config`'s credential
	// controls, so a bundled provider signs in here exactly as it does there.
	HostedAuthState(fieldId string) HostedAuthState
	// Step 2: poll until the user approves — the token lands in the credential
	// bag under `field_id` (memory only, like every retire credential) and the
	// field flips to `Connected` — or the attempt ends (`Failed`).
	HostedAuthWait(fieldId string)
	// The done state's by-hand list, one line per record: the ones still
	// pointing at the deleted box first, worded as urgent, then the stale
	// shared-name `TXT` (§ DNS cleanup). A single "nothing left" line when
	// the list is empty.
	LeftoverLines() []fauna_core.LocalizedText
	// Re-run after a failed step. Idempotent by construction: `find_records`
	// + `delete_record` + `delete_server` all converge, so a run that died
	// between the steps simply finishes. Acts only on the armed server,
	// and only while its run is on screen.
	Retry()
	// Select a row — in the list state only. Clears any code fetched for a
	// previously selected row — a code belongs to the domain it was fetched
	// for. Refused in every other phase, so nothing re-targets a confirm
	// summary or a run once it names a server.
	Select(serverId string)
	SelectProvider(providerId string)
	// The provider the credential form is for, once one is picked.
	SelectedProvider() *string
	// The typed-name gate (§ Confirm shape): the confirm button enables on an
	// **exact** match with the selected server's provider-side name, on every
	// provider — one uniform shape, and it is what discharges the mandatory
	// per-box confirm for unmarked (OVH) rows.
	SetConfirmName(typed string)
	SetCredentialField(fieldId string, value string)
	// Record this session's nest's IPv4 once the app has resolved it, and
	// re-mark any rows already listed. `None` clears the badge.
	SetCurrentIpv4(ipv4 *string)
	// Replace the held `fauna.state.dns` credentials (§ Credential stance,
	// preference (b)) with ones the app read after the page opened — the
	// same index-paired JSON bags as the constructor. Read at `verify`.
	SetHeldDnsJson(heldDnsProviderIds []string, heldDnsCredsJson []string)
	Snapshot() RetireSnapshot
	// A step row's name.
	StepLabel(step RetireStep) fauna_core.LocalizedText
	// Why a skipped step was skipped — `None` for any other state.
	StepNote(state StepState) *fauna_core.LocalizedText
	// The transfer-code line a row carries in place of (or beside) its
	// button: the lock-lifts date for `AvailableAfter`, the go-to-your-
	// registrar note for `Unsupported` on an attributed row, "asking" while
	// fetching. `None` when the button alone says it (`Idle`) or the code
	// itself is shown, and for an unattributed row (nothing to transfer).
	TransferCodeNote(serverId string) *fauna_core.LocalizedText
	// Verify the entered credential and list the fauna boxes in the account.
	Verify()
	// The selected provider's VPS credential fields — the same filter
	// `vps_config` renders (`OnboardingMachine::visible_vps_fields`), since
	// the retire view reuses those controls by id.
	VisibleFields() []FieldMetaPlain
}

// One instance per `nest_retire` page visit.
type NestRetireMachine struct {
	ffiObject FfiObject
}

// The binding-face constructor. Every input is optional — the page opens
// cold from `launch_retry` with none of it — and each held DNS credential
// arrives as the same `providers.yaml`-keyed JSON bag the wizard's
// dispatch already takes, so no new credential wire shape appears:
// `held_dns_provider_ids[i]` pairs with `held_dns_creds_json[i]`, one
// entry per `fauna.state.dns` credential (a pair missing its other half
// is dropped).
func NewNestRetireMachine(candidateDomains []string, currentIpv4 *string, heldDnsProviderIds []string, heldDnsCredsJson []string, providerBaseUrl *string) *NestRetireMachine {
	return FfiConverterNestRetireMachineINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_fauna_onboarding_machine_fn_constructor_nestretiremachine_new(FfiConverterSequenceStringINSTANCE.Lower(candidateDomains), FfiConverterOptionalStringINSTANCE.Lower(currentIpv4), FfiConverterSequenceStringINSTANCE.Lower(heldDnsProviderIds), FfiConverterSequenceStringINSTANCE.Lower(heldDnsCredsJson), FfiConverterOptionalStringINSTANCE.Lower(providerBaseUrl), _uniffiStatus)
	}))
}

// Add attribution candidates the app learned after the page opened — the
// admin entry's active local-domain list (§ DNS cleanup → *Several
// domains* (1)). Read at `verify`; a domain already held is not repeated.
func (_self *NestRetireMachine) AddCandidateDomains(domains []string) {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_add_candidate_domains(
			_pointer, FfiConverterSequenceStringINSTANCE.Lower(domains), _uniffiStatus)
		return false
	})
}

// The announced inputs have landed — or their read failed, which keeps
// what the page opened with. Verify is pressable again.
func (_self *NestRetireMachine) AppInputsLanded() {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_app_inputs_landed(
			_pointer, _uniffiStatus)
		return false
	})
}

// Back from a later phase to the one before it. From `List` this returns
// to the credential form; from `Confirm`, to the list. Either move
// disarms; a run in flight or failed is left by `cancel`, not by `back`.
func (_self *NestRetireMachine) Back() {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_back(
			_pointer, _uniffiStatus)
		return false
	})
}

// Move to the confirm state, computing the DNS plan the summary shows.
func (_self *NestRetireMachine) BeginConfirm() {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_begin_confirm(
			_pointer, _uniffiStatus)
		return false
	})
}

// Whether the verify button is pressable: a provider is picked, every
// required field is filled in, no listing is in flight, and no input the
// app announced is still on its way.
func (_self *NestRetireMachine) CanVerify() bool {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_can_verify(
			_pointer, _uniffiStatus)
	}))
}

// Back out of the page. **Drops the token** and every fetched code — the
// credential stance is that neither outlives the visit.
func (_self *NestRetireMachine) Cancel() {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_cancel(
			_pointer, _uniffiStatus)
		return false
	})
}

// Run the retire: **DNS first, then the server.**
//
// Refuses unless the typed-name gate is satisfied. A DNS-step failure
// stops the run *before the server is touched* and offers retry or
// *delete the server anyway* — never a silent continue, because
// continuing is what strands `A` records on an address about to be
// reassigned.
//
// Passing the gate **arms** the run to the selected server: `retry` and
// `force_server` act on that id and no other, so the typed name guards
// every door into `delete_server`, not only this one.
func (_self *NestRetireMachine) Confirm() {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_confirm(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

}

// The confirm summary (§ Confirm shape), one sentence per line, in the
// order the view shows them: what is deleted and where; the attributed
// domain and every other verified domain; that the data cannot be
// recovered except from a backup; on the current box, that this session's
// nest stops existing; and the DNS plan — the records removed first, or
// that none will be — followed by what is left to remove by hand.
//
// The `provider` argument of the first line is the provider's display
// **key**, so render it with nested resolution.
func (_self *NestRetireMachine) ConfirmSummary() []fauna_core.LocalizedText {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterSequenceLocalizedTextINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_confirm_summary(
				_pointer, _uniffiStatus),
		}
	}))
}

// The app is reading more inputs after the page opened (the admin
// entry's live custody read and local-domain list): hold verify until
// [`Self::app_inputs_landed`], because `verify` is where the credentials
// and candidates are read, and a verify ahead of them would clean less
// than the person's own config allows.
func (_self *NestRetireMachine) ExpectAppInputs() {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_expect_app_inputs(
			_pointer, _uniffiStatus)
		return false
	})
}

// Fetch the selected row's transfer authorization code. Both arms are
// rendered: the code, or the instant a registry lock lifts.
func (_self *NestRetireMachine) FetchTransferCode() {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_fetch_transfer_code(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

}

// Delete the server despite a failed DNS step (§ DNS cleanup: the failure
// branch offers *retry* or *delete the server anyway*). Acts only on the
// armed server, and only while the offer stands. The not-removed plan
// lines join the by-hand list; the dangling-record warning is the view's
// to repeat.
func (_self *NestRetireMachine) ForceServer() {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_force_server(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

}

// Step 1 of a `hosted-auth` field's sign-in: POST the device-authorization
// request to the form's `base-url`. `Some` carries what the app opens and
// shows; `None` means the attempt failed and the field's state says why.
// Follow with [`Self::hosted_auth_wait`].
func (_self *NestRetireMachine) HostedAuthBegin(fieldId string) *HostedAuthPrompt {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	res, _ := uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_onboarding_machine_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) *HostedAuthPrompt {
			return FfiConverterOptionalHostedAuthPromptINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_hosted_auth_begin(
			_pointer, FfiConverterStringINSTANCE.Lower(fieldId)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_rust_buffer(handle)
		},
	)

	return res
}

// The `hosted-auth` button's label — the wizard's one mapping.
func (_self *NestRetireMachine) HostedAuthButtonText(fieldId string) string {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_hosted_auth_button_text(
				_pointer, FfiConverterStringINSTANCE.Lower(fieldId), _uniffiStatus),
		}
	}))
}

// Whether the sign-in button is pressable: the form's `base-url` is
// filled in and no attempt on this field is mid-flight.
func (_self *NestRetireMachine) HostedAuthCanBegin(fieldId string) bool {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_hosted_auth_can_begin(
			_pointer, FfiConverterStringINSTANCE.Lower(fieldId), _uniffiStatus)
	}))
}

// Where a `hosted-auth` credential field's sign-in stands — `Idle` for a
// field never started. The retire view reuses `vps_config`'s credential
// controls, so a bundled provider signs in here exactly as it does there.
func (_self *NestRetireMachine) HostedAuthState(fieldId string) HostedAuthState {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterHostedAuthStateINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_hosted_auth_state(
				_pointer, FfiConverterStringINSTANCE.Lower(fieldId), _uniffiStatus),
		}
	}))
}

// Step 2: poll until the user approves — the token lands in the credential
// bag under `field_id` (memory only, like every retire credential) and the
// field flips to `Connected` — or the attempt ends (`Failed`).
func (_self *NestRetireMachine) HostedAuthWait(fieldId string) {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_hosted_auth_wait(
			_pointer, FfiConverterStringINSTANCE.Lower(fieldId)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

}

// The done state's by-hand list, one line per record: the ones still
// pointing at the deleted box first, worded as urgent, then the stale
// shared-name `TXT` (§ DNS cleanup). A single "nothing left" line when
// the list is empty.
func (_self *NestRetireMachine) LeftoverLines() []fauna_core.LocalizedText {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterSequenceLocalizedTextINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_leftover_lines(
				_pointer, _uniffiStatus),
		}
	}))
}

// Re-run after a failed step. Idempotent by construction: `find_records`
// + `delete_record` + `delete_server` all converge, so a run that died
// between the steps simply finishes. Acts only on the armed server,
// and only while its run is on screen.
func (_self *NestRetireMachine) Retry() {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_retry(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

}

// Select a row — in the list state only. Clears any code fetched for a
// previously selected row — a code belongs to the domain it was fetched
// for. Refused in every other phase, so nothing re-targets a confirm
// summary or a run once it names a server.
func (_self *NestRetireMachine) Select(serverId string) {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_select(
			_pointer, FfiConverterStringINSTANCE.Lower(serverId), _uniffiStatus)
		return false
	})
}

func (_self *NestRetireMachine) SelectProvider(providerId string) {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_select_provider(
			_pointer, FfiConverterStringINSTANCE.Lower(providerId), _uniffiStatus)
		return false
	})
}

// The provider the credential form is for, once one is picked.
func (_self *NestRetireMachine) SelectedProvider() *string {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_selected_provider(
				_pointer, _uniffiStatus),
		}
	}))
}

// The typed-name gate (§ Confirm shape): the confirm button enables on an
// **exact** match with the selected server's provider-side name, on every
// provider — one uniform shape, and it is what discharges the mandatory
// per-box confirm for unmarked (OVH) rows.
func (_self *NestRetireMachine) SetConfirmName(typed string) {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_set_confirm_name(
			_pointer, FfiConverterStringINSTANCE.Lower(typed), _uniffiStatus)
		return false
	})
}

func (_self *NestRetireMachine) SetCredentialField(fieldId string, value string) {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_set_credential_field(
			_pointer, FfiConverterStringINSTANCE.Lower(fieldId), FfiConverterStringINSTANCE.Lower(value), _uniffiStatus)
		return false
	})
}

// Record this session's nest's IPv4 once the app has resolved it, and
// re-mark any rows already listed. `None` clears the badge.
func (_self *NestRetireMachine) SetCurrentIpv4(ipv4 *string) {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_set_current_ipv4(
			_pointer, FfiConverterOptionalStringINSTANCE.Lower(ipv4), _uniffiStatus)
		return false
	})
}

// Replace the held `fauna.state.dns` credentials (§ Credential stance,
// preference (b)) with ones the app read after the page opened — the
// same index-paired JSON bags as the constructor. Read at `verify`.
func (_self *NestRetireMachine) SetHeldDnsJson(heldDnsProviderIds []string, heldDnsCredsJson []string) {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_set_held_dns_json(
			_pointer, FfiConverterSequenceStringINSTANCE.Lower(heldDnsProviderIds), FfiConverterSequenceStringINSTANCE.Lower(heldDnsCredsJson), _uniffiStatus)
		return false
	})
}

func (_self *NestRetireMachine) Snapshot() RetireSnapshot {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterRetireSnapshotINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_snapshot(
				_pointer, _uniffiStatus),
		}
	}))
}

// A step row's name.
func (_self *NestRetireMachine) StepLabel(step RetireStep) fauna_core.LocalizedText {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	return fauna_core.FfiConverterLocalizedTextINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_step_label(
				_pointer, FfiConverterRetireStepINSTANCE.Lower(step), _uniffiStatus),
		}
	}))
}

// Why a skipped step was skipped — `None` for any other state.
func (_self *NestRetireMachine) StepNote(state StepState) *fauna_core.LocalizedText {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalLocalizedTextINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_step_note(
				_pointer, FfiConverterStepStateINSTANCE.Lower(state), _uniffiStatus),
		}
	}))
}

// The transfer-code line a row carries in place of (or beside) its
// button: the lock-lifts date for `AvailableAfter`, the go-to-your-
// registrar note for `Unsupported` on an attributed row, "asking" while
// fetching. `None` when the button alone says it (`Idle`) or the code
// itself is shown, and for an unattributed row (nothing to transfer).
func (_self *NestRetireMachine) TransferCodeNote(serverId string) *fauna_core.LocalizedText {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalLocalizedTextINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_transfer_code_note(
				_pointer, FfiConverterStringINSTANCE.Lower(serverId), _uniffiStatus),
		}
	}))
}

// Verify the entered credential and list the fauna boxes in the account.
func (_self *NestRetireMachine) Verify() {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_verify(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

}

// The selected provider's VPS credential fields — the same filter
// `vps_config` renders (`OnboardingMachine::visible_vps_fields`), since
// the retire view reuses those controls by id.
func (_self *NestRetireMachine) VisibleFields() []FieldMetaPlain {
	_pointer := _self.ffiObject.incrementPointer("*NestRetireMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterSequenceFieldMetaPlainINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_nestretiremachine_visible_fields(
				_pointer, _uniffiStatus),
		}
	}))
}
func (object *NestRetireMachine) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterNestRetireMachine struct{}

var FfiConverterNestRetireMachineINSTANCE = FfiConverterNestRetireMachine{}

func (c FfiConverterNestRetireMachine) Lift(handle C.uint64_t) *NestRetireMachine {
	result := &NestRetireMachine{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_fauna_onboarding_machine_fn_clone_nestretiremachine(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_fauna_onboarding_machine_fn_free_nestretiremachine(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*NestRetireMachine).Destroy)
	return result
}

func (c FfiConverterNestRetireMachine) Read(reader io.Reader) *NestRetireMachine {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterNestRetireMachine) Lower(value *NestRetireMachine) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*NestRetireMachine")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterNestRetireMachine) Write(writer io.Writer, value *NestRetireMachine) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalNestRetireMachine(handle uint64) *NestRetireMachine {
	return FfiConverterNestRetireMachineINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalNestRetireMachine(value *NestRetireMachine) uint64 {
	return uint64(FfiConverterNestRetireMachineINSTANCE.Lower(value))
}

type FfiDestroyerNestRetireMachine struct{}

func (_ FfiDestroyerNestRetireMachine) Destroy(value *NestRetireMachine) {
	value.Destroy()
}

// The single shared onboarding wizard state machine. Each app holds
// `Arc<OnboardingMachine>`, observes via the registered `OnboardingObserver`,
// and mutates via the methods exposed below.
//
// Wizard state is in-memory only — there is no persistence layer here.
// Identity-confirmation methods return the secret hex so the per-app
// glue can write it to its long-term store immediately, and `seed_identity`
// lets app-launch code pre-populate the wizard at HandleEntry from a
// previously-stored secret.
//
// Uses `std::sync::Mutex` (not `tokio::sync::Mutex`) so getters and sync
// mutations work from any thread context — including UI threads on native
// apps and `#[tokio::test]` runtimes. Async methods take a snapshot under
// the lock, drop the lock, do IO, then re-acquire to apply changes; the lock
// is never held across an `await`.
type OnboardingMachineInterface interface {
	// The "Almost ready" surface's explicit exit — **"Use a different nest"**
	// (`onboarding-provisioning.md` § "Almost ready" surface → *Exit*): the way
	// out for a box that will never answer (a `create_server` that failed after
	// the slot was written, a box deleted at the provider), without which a
	// resumed launch is pinned on a waiting page for ever.
	//
	// Clears the awaiting slot of the identity being onboarded (through the
	// injected [`PendingProvisionStore`], addressed by that identity's secret —
	// on an append run the active account is a different one) and lands the wizard
	// at `HandleEntry` **holding the same identity**: the user is choosing a
	// different nest, not a different self. The landing is `launch_retry`'s
	// fallthrough (`seed_identity`) with the surface's state — outcome, snapshot,
	// provisioning run, reach override — cleared behind it; the machine's
	// in-memory pending-provision row survives, as it does every `reset()`, so
	// choosing the SAME domain again resumes the box rather than minting a
	// second one (§ 6 *The pending-provision slot*).
	//
	// The slot is cleared BEFORE the state moves: a crash between the two
	// relaunches onto the surface with the exit still on it, which is a
	// recoverable place, where the other order would land a slot-less identity
	// on a wizard the user never asked for.
	//
	// No-op unless `wizard_outcome()` is `AwaitingManualDns` — a stray call must
	// not retire a resumable box's slot — and while a claim is in flight
	// ([`Self::awaiting_dns_fallthrough_enabled`]), which would race the claim's
	// own nest-binding write.
	AbandonAwaitingManualDns()
	// The age claim the glue set, if any — verbatim, attestation included
	// (the iOS round reads it to decide a re-mint). What actually rides the
	// wire is [`Self::age_claim_to_send`].
	AgeClaim() *AgeClaimPlain
	// SHA-256 of [`Self::age_claim_message`] — the ONE value both platform
	// attestations bind: iOS hands it to App Attest as `clientDataHash`;
	// android passes it base64url-unpadded as the Play Integrity classic
	// request's nonce. A digest on purpose: Apple's and Google's logs see
	// 32 opaque bytes, never the band or the actor id in the pre-image. The
	// nest recomputes exactly this on both arms.
	AgeClaimDigest(nonceHex string, band string, applicationId string) ([]byte, error)
	// The exact bytes the platform attestation must commit to —
	// `fauna_protocol::age::age_claim_signed_message(nonce, band,
	// application_id, actor_id)` for the wizard's current identity. What
	// the platforms actually consume is their SHA-256,
	// [`Self::age_claim_digest`]; this is the pre-image, for transparency
	// and tests. One definition, shared with the nest's verifier.
	AgeClaimMessage(nonceHex string, band string, applicationId string) ([]byte, error)
	// Whether the "Almost ready" surface's "Copy all" button has anything to
	// copy. False in the records-less mode — the resumed standard path, whose
	// DNS was ours to write — where [`Self::awaiting_dns_records_text`] is
	// empty and a click would copy nothing.
	//
	// A getter rather than a rule each app applies to `dns_records`, the same
	// division `awaiting_dns_records_text` above already makes for the text
	// itself: the app asks, the machine decides.
	// See [`crate::snapshots::awaiting_manual_dns::copy_all_enabled`].
	AwaitingDnsCopyEnabled() bool
	// Whether the "Almost ready" surface's exit ("Use a different nest") may be
	// taken now — off only while a claim is in flight. The app asks, the machine
	// decides, exactly as for [`Self::awaiting_dns_copy_enabled`].
	// See [`crate::snapshots::awaiting_manual_dns::fallthrough_enabled`].
	AwaitingDnsFallthroughEnabled() bool
	// The deferred-DNS records as the exact JSON the awaiting-DNS slot carries:
	// `serde_json::to_string(&dns_records)` over the wizard's
	// `Vec<DnsRecordPlain>` — the `dns_records_json` field of
	// `fauna_launch_machine::AwaitingDnsRecord`.
	//
	// **Exists so no non-Rust client ever hand-rolls that JSON.** The seeder
	// (`seed_awaiting_manual_dns`) parses it back with serde, which expects
	// serde's field names (`record_type`, …) — but the UniFFI and WASM bindings
	// expose the record as `recordType`. A client-side `Gson`/`JSON.stringify`
	// of the *bound* type therefore produces JSON that deserializes to an empty
	// list, silently losing the records the user still has to add at their
	// registrar — a failure that only shows up after a relaunch. Rust clients
	// call `serde_json::to_string` directly and get byte-identical output.
	AwaitingDnsRecordsJson() string
	// The records the user must add at their registrar, formatted for display —
	// one line per record. Every app renders the "Almost ready" records from
	// this, and copies *this* to the clipboard, so the label and the copy button
	// can never disagree and all seven apps give the same instruction.
	// See [`crate::snapshots::awaiting_manual_dns::format_dns_records`].
	AwaitingDnsRecordsText() string
	// Snapshot for the post-provisioning "Almost ready" surface. Pure read;
	// cheap clone. The client renders this on every observer tick while
	// `wizard_outcome()` is `AwaitingManualDns`. Per
	// `docs/goal/behavior/onboarding.md` § "Wizard exit handling".
	AwaitingManualDnsSnapshot() AwaitingManualDnsSnapshot
	// Stage-aware Back button. The wizard's nav graph is small enough to
	// hardcode here so each app doesn't replicate the routing.
	Back()
	// Enter `identity_created`. Entry **mints, and does not commit.**
	//
	// The origin is deliberately *not* written here. [`State::canonical_secret`]
	// reads it as *"the screen the user committed on"* (`onboarding.md`
	// § 1 Identity), and this screen's entry fills its own slot — so an
	// entry-time write would make mere curiosity outrank an identity the user
	// already pasted and confirmed: import the real key, tap "create" to look
	// around, go Back, and every authenticating call signs as the throwaway
	// while the wizard terminal persists it (§ Long-term store contract).
	// [`Self::begin_import_identity`] *can* afford the entry-time write because
	// it leaves its slot empty, so "origin says `Imported`, slot is empty" is a
	// legible back-out state the fallback arm covers; a minted-on-entry slot
	// has no such tell. The commit point is [`Self::confirm_generated_identity`].
	//
	// The mint is guarded for the same reason the recovery-kit root is
	// (see [`Self::confirm_generated_identity`]): re-entry after
	// back-navigation must re-show the SAME key, or a key the user was just
	// told to write down is silently invalidated.
	BeginCreateIdentity()
	BeginImportIdentity()
	// [`Self::begin_import_identity`], carrying the reason the user was *sent*
	// there — for arrivals the user did not ask for.
	//
	// The launch flow's `superseded` refusal is the case that needs it
	// (`identity-succession.md` § Propagation → *Own device fleet*: the client
	// "surfaces 'this identity was succeeded — import the new identity'"): the
	// affordance IS the import screen, so the only thing distinguishing it from
	// a user who chose to import is the explanation on that page's existing
	// `error-message`. `recovery_entry`'s own superseded refusal
	// (`onboarding.md` § 1 Identity) routes the same way.
	//
	// **Why the reason lives in machine state rather than at the call site.**
	// The per-app onboarding views mirror `error_message()` reactively on every
	// observer tick (linux's `handle_change` re-reads it into the GTK label each
	// time), so a reason written straight to a widget is erased by the next tick
	// — including the tick this very transition fires. Setting both fields under
	// one [`Self::mutate`] also makes the pair atomic: no observer ever sees the
	// import step without the explanation that justifies it.
	BeginImportIdentityWithReason(reason string)
	// `recover-lost-box-button` on `identity_choice` (the fresh re-onboarded
	// client entry). Marks recovery intent and routes to `identity_import`
	// first — the admin's identity must be loaded to read the
	// `fauna.state.deployment-seeds` map) — then `confirm_imported_identity` lands on
	// `NestRecovery` (the `condition: "recovery-intent"` transition).
	BeginRecoverLostBox()
	// `restore-from-recovery-kit-button` on `identity_choice` — the
	// phrase-only IDENTITY restore (`onboarding.md` § 1 Identity). Routes to
	// the `recovery_entry` screen. Distinct from `begin_recover_lost_box`,
	// which starts the total-box-loss NEST recovery branch.
	BeginRecoveryEntry()
	// The `nest_provisioning` page's top-region price summary — up to two
	// line items: the domain's one-time registration price (only when the
	// wizard is buying a new domain) and the selected VPS's monthly price
	// (always present — `vps_config`'s Continue requires a selection).
	// Both prices were already shown and, for the domain, explicitly
	// agreed to earlier in the wizard (`dns-tld-price-display` /
	// `dns-price-confirm-checkbox` on `dns_config`, the server-type radio
	// options on `vps_config`); this is a pre-commit recap before
	// `provisioning-start-button`, not a new price source. Pure
	// computation over already-in-state DNS/VPS data — no IO.
	// `docs/goal/behavior/onboarding.md` §6.
	BillOfMaterials() []BillOfMaterialsItem
	// `bill_of_materials()`'s non-recurring (domain) item, pre-folded into
	// one [`LocalizedText`] ready for `resolve_nested` — `None` when
	// nothing is chargeable. `{label}` carries the step's own i18n key
	// (never its own args — `step_label` is always a bare key — so passing
	// it as a `resolve_nested` arg is safe) and `{price}`/`{renewal}` the
	// shared [`crate::helpers::format_price`] output. Picks the
	// `bom_line_domain` key over plain `bom_line` when the registrar
	// quoted a renewal price (`onboarding.md` § 6: disclosed before the
	// charge). Lifted out of per-app derivation — linux and tui carried
	// mirrored, independently-drifting copies of this exact branch.
	BomDomainLine() *fauna_core.LocalizedText
	// `bill_of_materials()`'s recurring (VPS) item, pre-folded into one
	// [`LocalizedText`] ready for `resolve_nested` — `None` when nothing is
	// selected yet. Always the `bom_line_recurring` key: VPS pricing has no
	// first-year/renewal split. See [`Self::bom_domain_line`] for the
	// `{label}`-is-a-key rationale.
	BomVpsLine() *fauna_core.LocalizedText
	// Whether the deployment's **calendar (CalDAV)** subsystem should be
	// enabled at claim. Machine-derived sibling of
	// [`Self::email_enable_requested`] — same two-axis derivation
	// (handle locality AND NAT axis), no checkbox (`onboarding.md` § 3b; the
	// DAV enables also mint the shared MSEK when first, so the home-relay
	// divergence argument covers them too).
	//
	// The per-app launch glue calls `MailAdminClient::set_caldav_enabled(true)`
	// on it at `LoggedIn`. The subsystems stay **separately gated** (CalDAV needs
	// only the HTTPS surface — no MX/DKIM — so it can be on where email is off);
	// they merely share this default today. Per
	// `docs/goal/behavior/caldav-server.md` § Independent enablement.
	CaldavEnableRequested() bool
	CanContinueDns() bool
	// Wizard-exit Continue button enables — provisioning Succeeded.
	CanContinueProvisioning() bool
	CanContinueVps() bool
	// Retry button is shown — the run Failed or was soft-Cancelled.
	CanRetryProvisioning() bool
	CanVerifyDns() bool
	CanVerifyVps() bool
	CancelHandleCheck()
	CancelInviteOp()
	// Sets the cancel flag. The running provisioning task observes it at
	// the next step boundary or retry iteration. Soft-cancel only —
	// already-created VPS/DNS resources stay; a subsequent retry picks
	// them up via idempotency.
	CancelProvisioning()
	// The DNS-provider credential captured at the onboarding DNS step, in the
	// shape the launched client's `DnsManagementMachine::PutCredentials`
	// consumes. `None` unless a provider was selected, `verify_dns()`
	// succeeded, and the admin did **not** choose "set up later" — i.e. there
	// is a verified credential worth sealing.
	//
	// This is the onboarding→launch **hand-off channel** for the client-side
	// DNS credential store (`docs/goal/behavior/dns-management.md` § Where the
	// credential lives; `docs/goal/behavior/onboarding.md` § 4). The
	// onboarding machine has no account-plane write capability and, in the
	// fresh-provision path, no live nest "at the end of the DNS step"; so
	// rather than sealing here it exposes the captured credential and the
	// **launched client** seals it once authenticated, via the normal
	// `PutCredentials` path — one store, one writer (the
	// `DnsManagementMachine`), no second config-put path in onboarding. The
	// per-app launch glue reads this at `wizard_outcome() == LoggedIn` and
	// dispatches `PutCredentials { provider_id, fields, label }` through
	// `build_dns_management_machine_with_credentials`. The machine re-runs
	// `verify()` to (re)derive covered zones, so the captured zones are not
	// re-surfaced here.
	CapturedDnsCredential() *CapturedDnsCredential
	// Whether the deployment's **contacts (CardDAV)** subsystem should be
	// enabled at claim. Machine-derived sibling of
	// [`Self::caldav_enable_requested`] — same two-axis derivation (handle
	// locality AND NAT axis), no checkbox (`onboarding.md` § 3b).
	//
	// The per-app launch glue calls `MailAdminClient::set_carddav_enabled(true)`
	// on it at `LoggedIn` and, when neither email nor CalDAV minted the shared
	// MSEK, provisions the CardDAV-only mailbox
	// (`enable_carddav_mailbox_with_generated_password`). Per
	// `docs/goal/behavior/carddav-server.md` § Independent enablement.
	CarddavEnableRequested() bool
	// The claim code a client wants the `ClaimCode` page input pre-filled with
	// (the factory-reset re-onboard path), or `None` for the ordinary path
	// where the human types it. See
	// [`navigate_to_claim_code_for_known_nest_with_code`].
	ClaimCodePrefill() *string
	// Snapshot for the `claim_code` page. Pure read; cheap clone of a
	// small enum + `LocalizedText`. Per
	// `docs/goal/behavior/onboarding.md` §3a — clients render this on every
	// observer tick.
	ClaimCodeSnapshot() ClaimCodeSnapshot
	ClearError()
	Complete(outcome HandleCheckOutcome, msgKey string, args map[string]string)
	CompleteProbeError(phase HandleCheckPhase, transient bool, cause string)
	ConfirmGeneratedIdentity() (string, error)
	ConfirmImportedIdentity(secret string) (string, error)
	// Records the user's explicit acceptance of the displayed registration
	// price. Required before `start_provisioning()` will run the buy-domain
	// path. Pair with `dns_status_text()` (or its i18n key variant) so the
	// UI shows the user the exact price before they consent.
	ConfirmPrice()
	// `recovery-kit-confirm-button`: the user says the phrase is saved.
	// Advances to `HandleEntry`; the pending root is KEPT for the signed-in
	// handoff, where the per-app glue takes it
	// ([`Self::take_pending_recovery_secret`]) and runs registration + escrow.
	ConfirmRecoveryKit()
	// Sync transition. The actual buy / register happens in
	// `start_provisioning()` so the user can still back out before
	// committing money.
	ContinueFromDns() error
	// Continue button on the `dns_post_instructions` page. Sets
	// `wizard_outcome()` to `AwaitingManualDns { nest_url, dns_records,
	// claim_code }` and returns `OnboardingStep::Done`. Caller is
	// responsible for navigating to the "Almost ready" surface.
	ContinueFromDnsPostInstructions() OnboardingStep
	// Bottom-row Continue on the `nest_provisioning` page. Advances only when
	// the provisioning snapshot is `Succeeded`; otherwise returns the current
	// step unchanged so the click is a no-op.
	//
	// On the deferred-DNS path it transitions to `DnsPostInstructions`. On the
	// standard path it lands on **`NatModeChoice`** (§ 3b-bis), exactly as a
	// claim-code submit does — this page never exits straight to
	// `Done`/`LoggedIn` (`docs/goal/behavior/onboarding.md` § 6, ratified
	// 2026-08-29). `Succeeded` there means the box is built *and claimed* (the
	// `Online` claiming substep, `run_provisioning_claim`), so the § 3b-bis /
	// § 3b-ter tail owns the exit from here on. The pre-ratification `LoggedIn` /
	// `Done` exit signed the user in to a box **nobody had claimed** and then
	// bounced them to a claim page asking for a code they never saw.
	//
	// The standard path additionally requires the claim to have actually
	// completed. That is not belt-and-braces: the orchestrator marks the run
	// `Succeeded` and notifies before returning, so an app that paints and takes
	// a click inside the window before `set_claiming` reopens the step could
	// otherwise Continue past an unclaimed box. The gate is the claim's own
	// state, not a timing assumption.
	ContinueFromProvisioning() OnboardingStep
	// VPS-stage Continue button (`vps-config-continue-button`). Validates
	// the form (verified credentials, location chosen, server type
	// chosen) and transitions the wizard to `NestProvisioning`. The
	// orchestrator is kicked separately by the architecture-defined
	// "Buy and set up" CTA on the `nest_provisioning` page, which calls
	// `start_provisioning()` — that gives the user a final price-review
	// gate before money is committed.
	ContinueFromVps() error
	CurrentHandle() string
	// Wire-up for the `nat-mode-defer-button`: exit the `nat_mode_choice`
	// page without committing. The seeded mode stays in effect server-side
	// (already a working default — nothing is sent); the admin can set it
	// later from the admin panel. The wizard exits exactly as a successful
	// submit does: `wizard_outcome() == LoggedIn`, step → `Done`. Per
	// `docs/goal/behavior/onboarding.md` § 3b-bis.
	DeferNatModeChoice() OnboardingStep
	DnsConfig() DnsConfigState
	// Renders the captured DNS records as the markdown table the
	// `dns_post_instructions` page surfaces — same shape the orchestrator's
	// `DeferredDnsResult.instructions_markdown` produces. Returns `None`
	// when records or the provisioning result aren't populated yet
	// (i.e. the deferred-DNS run hasn't finished). Clients that prefer to
	// render their own table can read `dns_records()` and the snapshot's
	// result directly.
	DnsPostInstructions() *string
	// Whether the DNS-provider button for `provider_id` should be selectable
	// given the user's current DNS choices. A provider is *ineligible* when
	// the user wants to buy a domain but it can't register
	// (`buy_domain && !Registrar`), or wants one provider for both DNS and
	// VPS but it has no VPS capability (`same_provider_for_vps && !Vps`).
	// Unknown ids are never eligible. This is the queryable form of the
	// deselect-on-toggle guards in `toggle_buy_domain` /
	// `toggle_same_provider_for_vps`; all apps drive per-provider button
	// sensitivity through it instead of re-deriving the capability rule.
	DnsProviderEligible(providerId string) bool
	// Why the `dns-provider-row[<id>]` control is not selectable — `None` when
	// it is, else the i18n key of a one-line explainer naming the constraint
	// that closed it *and the checkbox that re-opens it*.
	//
	// **This exists because a disabled control owes the user a reason**
	// (`ui/README.md` § Copy comprehensibility rule 5 — the cross-app owner;
	// `apps/tui.md` § Rendering → *Control vocabulary* rule 3 restates it for
	// the terminal, where DIM is the only other signal). [`Self::
	// dns_provider_eligible`] answers yes/no, which is enough to grey a row
	// out and not enough to explain it; a shell that wanted the reason would
	// have to re-derive the capability rule this machine owns, which is
	// exactly what priority #2 forbids. So the reason ships beside the
	// verdict, from the same shortfall computation.
	//
	// Note the deliberate asymmetry with `dns_provider_eligible` for an id
	// outside `PROVIDERS`: that id is *ineligible* but has no row on screen,
	// and there is no control to explain, so there is no reason to paint.
	DnsProviderIneligibleReason(providerId string) *fauna_core.LocalizedText
	// Returns the DNS records the deferred-DNS orchestrator captured.
	// Empty until `start_provisioning` runs the deferred path successfully.
	// Read by clients on the `dns_post_instructions` page.
	DnsRecords() []DnsRecordPlain
	DnsSetUpLater()
	// i18n-aware variant of the DNS status text. Returns the i18n key plus
	// the substitution map. Clients pass `(key, args)` through their
	// platform's localization pipeline (Apple `Bundle.main.localizedString`,
	// Android `getString`, Web `L()`, etc.). The string keys live in
	// `i18n/strings/en.yaml` under `onboarding.dns_config.status_*`.
	DnsStatusTextKey() fauna_core.LocalizedText
	// Derived view over `handle_check_snapshot().outcome` — NOT stored
	// state. `DomainAvailable` ⟺ `Unregistered`, `RegisteredNoNest` ⟺
	// itself; every other outcome (incl. `None` after a reset) has no
	// domain-registration verdict to report. Sound because the outcome and
	// this view share exactly one reset point (`reset_handle_check`, fired
	// on identity change) and the outcome is otherwise stable for the
	// lifetime of the DnsConfig step (`submit_handle_check_continue` reads
	// it once to decide the transition and never mutates it further).
	DomainStatus() *fauna_provisioning.DomainStatus
	// Test-aware nest base URL: the override map wins
	// when set; otherwise the wizard's state.nest_url is used.
	//
	// Use only for HTTP request URLs. For outcome values that identify
	// the nest to per-app glue (e.g. `WizardOutcome::AwaitingManualDns`),
	// read `state.nest_url` directly — the override must not leak into
	// persisted outcome data.
	EffectiveNestUrl() string
	// The identity the wizard is acting as — the secret every authenticating
	// call signs with, and the one the app's wizard terminal persists
	// (`onboarding.md` § Long-term store contract). `None` until the user has
	// committed an identity. The rule (origin decides, other slot as fallback)
	// and why it is a rule rather than a fixed precedence: [`State::canonical_secret`].
	EffectiveSecret() *string
	// Whether the deployment's **mail** subsystem should be enabled at claim.
	//
	// **Machine-derived — there is no checkbox** (`onboarding.md` § 3b: the
	// four claim-time enablement intents survive the retired
	// `encryption_mode_choice` page as derived defaults, *not relocated* onto
	// § 3b-bis, which stays confirm-only by design). The admin's change surface
	// after onboarding is the admin-mail page.
	//
	// The value is the conjunction of two axes (ratified 2026-07-13): the
	// handle-locality predicate ([`Self::handle_targets_real_domain`] — OFF
	// for `user@localhost` / `user@IP`, which cannot hold MX/DKIM/TLS) AND
	// the NAT axis ([`Self::effective_node_mode`]` != Private`). The NAT
	// conjunct is how the two-box home-relay deployment
	// (`deployment-home-with-public-relay.md`) says "this box runs no mail"
	// with no checkbox: both boxes share one real-domain handle, but the home
	// box is the one on the private axis — a claim-time enable there would
	// mint a fresh MSEK, diverging from the fleet MSEK `LinkBoth` re-seals
	// onto it. Still the onboarding→launch **hand-off channel**: the
	// per-app launch glue reads this once `wizard_outcome() == LoggedIn` and,
	// with its now-authenticated Admin client, calls
	// `MailAdminClient::set_mail_enabled(true)` — keeping the Admin-class gate
	// intact rather than opening a pre-identity admin surface. Idempotent with
	// the mail-settings enable path; only meaningful on the admin-claim path.
	// Per `docs/goal/behavior/mail-bridge-lifecycle.md` § Default-off on first
	// claim.
	EmailEnableRequested() bool
	ErrorMessage() *string
	// Log a refused restore and hand the outcome back unchanged.
	//
	// The step is left alone on purpose: every refusal here is something the
	// user acts on *from this screen* (fix the account, paste a different
	// phrase, retry) — except `Superseded`, whose routing the client owns.
	//
	// It deliberately does **not** write `error_message`. The outcome is the
	// contract, and the message that renders is the client's localized
	// resolution of it ([`Self::set_error_message`]) — writing an English
	// placeholder here would put a second, untranslated owner on the one
	// channel `error-message` reads.
	FailRecoveryEntry(outcome RecoveryEntryOutcome) RecoveryEntryOutcome
	GeneratedSecret() *string
	// `trust-box-grant-button`: the user trusts this box with the default
	// grant set. Latches the answer for the signed-in handoff — the wizard
	// holds no authenticated session, so it cannot mint here — and concludes
	// the wizard exactly as the NAT step would have.
	GrantDefaultTrust() OnboardingStep
	HandleCheckSnapshot() HandleCheckSnapshot
	// Step 1 of a `hosted-auth` field's sign-in: POST the device-authorization
	// request to the form's `base-url` and hand back what the app must open
	// (through its existing open-URL affordance) and show (the user code).
	// The field flips to `Pending`; follow with [`Self::hosted_auth_wait`].
	HostedAuthBegin(form CredentialForm, fieldId string) (HostedAuthPrompt, error)
	// The `hosted-auth` button's label for one field, derived purely from
	// [`Self::hosted_auth_state`] — the shared-Rust twin of the match arm
	// tui's `hosted_auth_button` and linux's `paint_hosted_auth_button` each
	// hand-copied and self-documented as a "mirror" of the other
	// (`onboarding.md` § 4). android/apple/web keep their own copy: each
	// binds a different platform i18n surface (`R.string.*`, a Swift
	// `LocalizedStringKey`, a reactive TS `t.*`), so the enum→label mapping
	// still has to be re-expressed per platform — only the two Rust-native
	// apps could actually call one function.
	HostedAuthButtonText(form CredentialForm, fieldId string) string
	// Whether the sign-in button is pressable: the form's `base-url` is
	// filled in and no attempt on this field is mid-flight. Owned here so no
	// app re-derives "which sibling field is the address" (priority #2).
	HostedAuthCanBegin(form CredentialForm, fieldId string) bool
	// Where a `hosted-auth` field's sign-in stands — the app's button label
	// source (`onboarding.md` § 4). `Idle` for a field never started.
	HostedAuthState(form CredentialForm, fieldId string) HostedAuthState
	// Step 2: poll the token endpoint at the server's interval until the user
	// approves (the token lands in the form's credential bag under
	// `field_id`, the field flips to `Connected`, and `can_verify_*` turns
	// true) or the attempt ends (`Failed`). Resolves only then — the app
	// awaits it the way it awaits `verify_dns`.
	HostedAuthWait(form CredentialForm, fieldId string) error
	IdentityOrigin() *IdentityOrigin
	InviteError(ctx ErrorContext, transient bool, cause string) OnboardingStep
	InviteRequestSnapshot() InviteRequestSnapshot
	IsBuyableViaProvider(domain string) bool
	IsLoading() bool
	LocalNestReachable() bool
	// Snapshot for the `nat_mode_choice` page. Pure read.
	// Per `docs/goal/behavior/onboarding.md` § 3b-bis.
	NatModeSnapshot() NatModeSnapshot
	// Lands the wizard on `ClaimCode` with `nest_url` + `handle`
	// pre-set and the snapshot reset to `Idle`. Per target
	// `docs/goal/behavior/onboarding.md` § App-launch routing — silent-challenge
	// fallback table (unclaimed-nest row): when /verify returns 404 AND
	// `setup-status.claimed == false`, the saved nest is up but unclaimed,
	// so the user must claim it themselves rather than ask for an invite.
	// Pure state mutation; no IO.
	NavigateToClaimCodeForKnownNest(nestUrl string, handle string)
	// Like [`navigate_to_claim_code_for_known_nest`], but also pre-loads a
	// claim `code` the client already holds so the `ClaimCode` page can
	// pre-fill its input (read via [`claim_code_prefill`]). Used by the
	// factory-reset re-onboard affordance: `fauna.admin.factory_reset` returns
	// the new claim code to the client and the human never sees it, so without
	// pre-fill the admin would land on the claim-code page with nothing to
	// type. After re-claim the nest is mode-unresolved, so the wizard still
	// runs claim-code → encryption-mode (per
	// `docs/goal/architecture/nest/storage-modes.md` § The claim-time choice).
	// Pure state mutation; no IO.
	NavigateToClaimCodeForKnownNestWithCode(nestUrl string, handle string, code string)
	// Pre-seed a pending invite at app launch when the per-app long-term
	// store has a record of one. Places the wizard at `InviteRequest` with
	// `nest_url`/`handle`/the supplied snapshot loaded. Per-app glue calls
	// this analogously to `seed_identity` after reading its store; the glue
	// is responsible for the navigation that puts the user on the
	// InviteRequest page.
	//
	// `status_json` is the JSON-serialized `InviteRequestState` enum from
	// the per-app store. The wizard parses it back into a typed snapshot.
	// If parsing fails (corrupt state, schema drift), the wizard falls back
	// to `InviteRequestState::PendingReview` with the supplied request_id —
	// the user can recheck and either resume or reset.
	// Lands the wizard on `InviteRequest` with `nest_url` + `handle`
	// pre-set and the snapshot reset to `Idle`. Per target
	// `docs/goal/behavior/onboarding.md` §"App-launch routing": when the
	// silent-challenge handshake reports the secret is unregistered on
	// an otherwise-running nest, the user needs an invite — drop them
	// directly on the invite-request page rather than make them
	// re-type their handle. Pure state mutation; no IO.
	//
	// Distinct from `seed_pending_invite` (which restores a previously
	// submitted request from the long-term store). Use this when there
	// is no prior request to resume.
	NavigateToInviteRequestForKnownNest(nestUrl string, handle string)
	NestUrl() string
	// The pending-invite resume slot for the current state, or `None` when
	// there is nothing to resume.
	//
	// **This replaced `submit_invite_request_continue()` (retired 2026-08-12).**
	// That method existed to *exit* the wizard with
	// `WizardOutcome::InviteSubmitted`, and the exit is what `onboarding.md`
	// § Wizard exit handling deletes: the pending-review journey never leaves
	// the `invite_request` page — it stays there and polls until the poll
	// resolves to `LoggedIn`. Apps call this at the
	// [`Self::wizard_submit_invite_request`] return instead, which § 3
	// Persistence callouts names as "the only write moment".
	//
	// Returning the assembled slot (rather than three getters) is deliberate:
	// the two rules that are silent when wrong — `state.nest_url` over
	// `effective_nest_url()`, and opaque `status_json` — live once here
	// instead of being re-derived by seven apps. See [`PendingInviteSlot`].
	PendingInviteSlot() *PendingInviteSlot
	// Serializes the current `InviteRequestState` for the per-app
	// pending-invite slot. The format is opaque to clients — the wizard
	// parses it back via `seed_pending_invite`. Returns `""` on the
	// unreachable case where serde fails (the enum derives Serialize).
	PendingInviteStatusJson() string
	// Pre-identity probe used by **app-launch routing** to discriminate
	// "secret unregistered + claimed nest → invite_request" from
	// "secret unregistered + unclaimed nest → claim_code" (per
	// `docs/goal/behavior/onboarding.md` § App-launch routing).
	//
	// Delegates to the active `NestApi`, so this rides the anonymous WS-RPC
	// connection just like the rest of the wizard — replacing the legacy
	// `GET /api/v1/setup-status` HTTP probe the web / launch-machine glue
	// used to call directly. The orchestrator
	// applies the safer-default fallback (`Err` → assume `claimed=true`);
	// surfacing the raw error here keeps that decision in one place per
	// caller rather than baking it into the machine.
	ProbeSetupStatusAt(nestUrl string) (SetupStatus, error)
	// Per-provider DNS-config status. Pure computation over the current
	// snapshot. UIs consume this via WASM/UniFFI and switch UI shape on
	// the variant; `can_continue_dns` consults it to decide whether
	// Continue is enabled.
	ProviderStatus() ProviderStatus
	// Whether the `vps-config-mail-mode-toggle` is ON. Returns the user's
	// explicit choice if set, else the handle's real-domain default
	// ([`Self::handle_targets_real_domain`]) — the same predicate that seeds the
	// §3b enable-email default, so a real-domain box defaults to a mail box and a
	// `localhost` / IP target defaults to social-only. Clients read this to
	// render the toggle's checked state and to gate the server-type radio (see
	// [`server_type_allowed_for_mail`]). Per `docs/goal/behavior/onboarding.md`
	// §5.
	ProvisionMailModeEnabled() bool
	// The box's reach address once `create_server` has returned, else `None`
	// (§ 6 *Reaching the box*). Survives a `reset()` no more and no less than
	// the rest of the run state does — see [`Self::reset`].
	ProvisionReachIpv4() *string
	// The selected update channel — the user's explicit choice if set, else
	// the default (`stable`). Clients read this to mark the selected
	// `vps-config-update-channel-row`.
	ProvisionUpdateChannel() fauna_provisioning.UpdateChannel
	// Why the wizard-exit Continue button is dead — `ui/README.md` rule 5:
	// the four `○` step glyphs are a symbol, not a reason. One message per
	// blocked `OverallStatus` (idle/running/failed/cancelled), because the
	// user's next act differs: start, wait, retry.
	ProvisioningContinueBlockedReason() *fauna_core.LocalizedText
	// Cancel button is shown — provisioning is actively running. Also gates
	// the elapsed-time ticker.
	ProvisioningInProgress() bool
	// Returns a clone of the current provisioning snapshot. Cheap (clones
	// a small struct). Pull-based — clients re-read on every observer
	// tick rather than receiving the snapshot via callback.
	//
	// The clone is `enrich_display`-ed so every app reads the canonical
	// per-step visibility booleans (`shows_substep`/`shows_error`/
	// `shows_attempt_suffix`) instead of re-deriving the rule. Computed here on
	// the outgoing clone — never on the live mutated state, which the
	// `set_cancelled` path mutates outside `with_step` (see `recompute_display`).
	ProvisioningSnapshot() fauna_provisioning.ProvisioningSnapshot
	RecheckInviteStatus() OnboardingStep
	// Polls the freshly-provisioned nest from the post-provisioning
	// "Almost ready" surface. The client calls this on a timer while
	// `wizard_outcome()` is `AwaitingManualDns` (the deferred-DNS exit),
	// single-shot — exactly like `recheck_invite_status`: one
	// `probe_setup_status` reachability+claim-status probe over WS-RPC, and
	// if the nest is reachable and unclaimed, one `claim_admin` call.
	//
	// Every call here is **pre-claim**: it rides the anonymous WS-RPC
	// connection — there is no authenticated actor session until the claim
	// succeeds. (A `setup-status` failure means DNS hasn't propagated yet or
	// the nest is still booting; both surface to the user as "waiting for
	// your nest to come online".) On a successful claim the wizard routes to
	// `NatModeChoice` — the same path `wizard_submit_claim_code` takes —
	// clearing the awaiting outcome. On the already-claimed resume it skips
	// that setup step and concludes through § 3b-ter's offer instead
	// (`TrustPrompt` on an app that renders it, else straight to `Done`).
	//
	// No-op (returns the current step) unless `wizard_outcome()` is
	// `AwaitingManualDns`. Transient failures (nest unreachable, 5xx on
	// claim) leave the snapshot `Pending` so the client keeps polling; a
	// rejected claim, a corrupt identity, or a first-contact identity
	// mismatch (security.md § Pre-claim surfacing) yields a terminal
	// `Error`. Per `docs/goal/behavior/onboarding.md` § "Wizard exit
	// handling".
	RecheckManualDns() OnboardingStep
	// `recover-method-cloud-button` on `nest_recovery`: re-provision the
	// selected box via a cloud VPS. Advances to `vps_config` in recovery mode
	// (the shared orchestrator re-provisions with the saved seed installed and
	// re-points A/AAAA as its `Dns` step — the drive is a later slice). Errors
	// if no box is selected.
	RecoverViaCloud() error
	// `recover-method-selfhosted-button` on `nest_recovery`: re-provision the
	// selected box via a self-hosted installer. Advances to
	// `recover_selfhosted_instructions` (the installer command carrying
	// `FAUNA_DEPLOYMENT_SEED`). Errors if no box is selected.
	RecoverViaSelfhosted() error
	// The custodied box list rendered on `nest_recovery` — one `nest_actor_id`
	// (hex) per box, empty when nothing is custodied / not yet synced
	// (`recover-box-empty-message`).
	RecoveryBoxes() []string
	// Which entry the recovery branch was reached from, or `None` outside it.
	// The glue uses this to route `recover-back-button` (came-from-launch tears
	// the wizard down to `launch_retry`; came-from-identity is `back()`).
	RecoveryCameFrom() *BoxRecoveryEntry
	// Whether the wizard is in the total-box-loss recovery branch. The web glue
	// gates the entry CTAs / recovery-mode provisioning on this.
	RecoveryIntent() bool
	// The minted-but-unregistered RecoveryKey root the `recovery_kit` screen
	// displays (64-hex + QR). `None` outside the kit screen's lifetime.
	// Hands out a plain `String` at the render boundary (the foreign string
	// is un-zeroizable by design — `fauna_core::secret` module docs); the
	// held copy stays the zeroizing, `Debug`-redacted [`SecretString`].
	RecoveryKitSecretHex() *string
	// The `fauna://recovery` URI behind the `recovery_kit` screen's QR **and**
	// its copy button — one payload for both (`identity-succession.md` § The
	// RecoveryKey, *Which encoding each affordance carries*); the display
	// alone is the bare [`Self::recovery_kit_secret_hex`]. It names the
	// account (the actor the generated identity derives to) and truthfully no
	// handle — none is chosen yet at this position, so a restore from it asks
	// (`recovery-entry-account-field`). `None` outside the screen's lifetime.
	// Built here so every app renders the same payload (priority #2).
	RecoveryKitUri() *string
	// The `nest_actor_id` (hex) of the selected box on `nest_recovery`, or
	// `None` before a selection. Drives the `recover-box-item` selected state.
	RecoverySelectedNestId() *string
	RedeemInvite() OnboardingStep
	// Mint the single-use nonce the platform attestation binds
	// (`fauna.account.age_nonce`, pre-identity, against the wizard's current
	// nest). The app then runs its attestation round over
	// [`Self::age_claim_message`] inside `expires_in_secs` and hands the
	// result to [`Self::set_age_claim`]. Minted from the nest the admission
	// will go to (`effective_nest_url`, the URL both admission paths use), and
	// remembered — with the platforms the reply listed — for the send guard.
	RequestAgeNonce() (AgeNoncePlain, error)
	// Discards all state and resets to `IdentityChoice`.
	//
	// Also clears the provisioning snapshot + cancel flag so a "start over"
	// (or the E2E `reset` between tests) returns to a clean Idle provisioning
	// page. Native apps reuse one long-lived machine across resets, so
	// without this a prior run's terminal snapshot (e.g. `Succeeded`) would
	// persist and hide the `provisioning-start-button` (visible only while
	// `overall == Idle`) — web avoids it only because its reset reconstructs
	// the machine. The claim-code snapshot is cleared for the identical reason:
	// its terminal `Claimed` state carries `submit_enabled == false`, so a
	// process that already claimed a nest would render the next claim's Submit
	// button permanently dead. (Its route in re-seeds too — see the
	// `UnregisteredUnclaimedNest` arm — but "start over" must not depend on
	// which door the user next walks through.) The remaining transient
	// snapshots (handle-check, invite) are re-seeded by their own setters at
	// the start of every use, so they don't need clearing here.
	Reset()
	// Disambiguate a `NotFound` recheck by asking whether this identity is now
	// *registered* on the nest — the "approval is detected as admission" rule
	// (`onboarding.md` § The pending-invite surface).
	//
	// The request row is gone either because the admin approved it (the approve
	// creates the account, then deletes the row) or because it was cancelled /
	// purged. Nothing in the row's absence separates those, and an anonymous
	// poller has no notification channel to be told which happened (every
	// notification plane is keyed on a bearer-proven `actor_id` this caller does
	// not have until approval creates it). So we run the same
	// `fauna.auth.{challenge,verify}` ceremony the silent sign-in uses, with the
	// identity we already hold:
	//
	// - **registered** → that verify *is* the login. Route to `LoggedIn`/`Done`
	// exactly like a redeem success — no "Approved, press Continue"
	// interstitial, mirroring the awaiting-DNS surface's auto-proceed.
	// - **not registered** → the request is genuinely gone: terminal
	// `invite.error.not_found`, on which the per-app glue deletes the slot.
	// - **couldn't tell** (unreachable, degraded nest, unusable secret) → stay
	// `PendingReview` and let the next poll tick ask again. Deliberately NOT
	// an `Error` state: `recheck_invite_status` only proceeds from
	// `PendingReview`, so writing any error here would wedge the poll
	// permanently — a dropped connection would strand a live request behind a
	// terminal, slot-deleting screen.
	ResolveNotFoundRecheck(nestUrl string, secret string, pendingRequestId string) OnboardingStep
	// The nest URL a restore connects to, resolved from the account handle's
	// domain exactly as the handle-check resolves its probe target: the local
	// / loopback classification first, then SRV port discovery for a public
	// name that carried no explicit port, so a clean `alice@domain` stays
	// port-hidden.
	//
	// The `"nest"` provider override wins when set, the same way
	// `run_handle_check_phases` lets it win over `target.base_url`. It has to
	// be resolved **here**, not left to `WsNestApi`: that type captures the
	// map once at construction, while the E2E bridge installs the override at
	// runtime through `set_provider_base_urls`, so a machine-side caller that
	// skips this step dials the resolved `https://` URL at a harness nest
	// serving plain HTTP (which surfaces as a corrupt-message WS handshake —
	// the failure this method's own tier_3 journey caught).
	ResolveRestoreTarget(domain string) string
	// Predecessor identity seeds a phrase-only restore recovered alongside the
	// account's own (`identity-succession.md` § Seed escrow). **Empty in every
	// ordinary onboarding**; non-empty only when the restored account is mid
	// corpus re-seal after an identity succession.
	//
	// The client persists these into its account registry beside the restored
	// identity — they are what lets the re-seal driver open a corpus still
	// sealed under a predecessor. Read at the same point as
	// [`Self::effective_secret`]: this machine holds no store, so an
	// unread value is simply lost when the wizard ends.
	RestoredPredecessors() []RestoredPredecessorSeed
	// Why the restore recovered no predecessor material even though the blob
	// carried a section — see
	// [`RecoveryEntryOutcome::RestoredPredecessorsLost`].
	RestoredPredecessorsUnreadable() *string
	// Re-runs provisioning from the top. Idempotency makes already-done
	// steps short-circuit on re-run, so this is functionally equivalent
	// to "resume from the failed step." Same fire-and-forget shape as
	// `start_provisioning`.
	//
	// Resuming means the SAME box: the re-run reuses the claim code and
	// expects the identity the box was built with (`pending_provision_row`),
	// rather than minting fresh ones a box that already exists can never
	// match. Only the run's own progress snapshot is reset here.
	RetryProvisioning()
	RunHandleCheckPhases(handle string)
	// Runs the four-step orchestrator **to completion** (the future
	// `start_provisioning` spawns) and stashes the terminal result. Observer
	// ticks drive re-render throughout; `provisioning_snapshot()` / the
	// `provisioning_cancel` flag stay readable/settable from another thread.
	//
	// `start_provisioning` is the fire-and-forget spawn wrapper used by web
	// (`spawn_local`) and any native caller already inside a tokio runtime.
	// Native apps that have **no** runtime on their UI thread during
	// onboarding (the GTK main thread on linux — `tokio::spawn` there would
	// panic) instead `await` this on a worker runtime
	// (`async_helper::run_on_tokio`). Idempotent like `start_provisioning`:
	// `run_provisioning_inner` resets the snapshot + cancel flag at entry, so
	// it doubles as the retry/resume entry point.
	RunProvisioning()
	// Place the wizard at `InviteRequest` with handle and nest_url
	// hydrated, but no pending-invite record. Used by the per-app
	// app-launch glue when the silent challenge reports the secret
	// isn't registered on a known-good nest — the user is still on
	// the right nest, just needs an invite. Equivalent in shape to
	// `seed_pending_invite` but without a `request_id` or status JSON.
	// Per the onboarding client target-state design's app-launch
	// failure-mode table (tracked internally).
	SeedAtInviteRequestUnregistered(handle string, nestUrl string)
	// App-launch hydration for the "Almost ready" surface. Lands the wizard
	// at the deferred-DNS exit from a previously-saved slot. The per-app
	// glue calls `seed_identity(secret)` first (so signing works for the
	// claim), then this with the persisted (nest_url, handle, dns_records,
	// claim_code). Sets `wizard_outcome()` to `AwaitingManualDns` — exactly
	// what the same-session `continue_from_dns_post_instructions` exit
	// produces — so the client renders the surface identically whether it
	// arrived this session or on relaunch. The client then polls
	// `recheck_manual_dns()`. Mirrors `seed_pending_invite` /
	// `seed_pending_encryption_mode_choice`. Per
	// `docs/goal/behavior/onboarding.md` § "Wizard exit handling".
	//
	// Clients holding the slot's opaque `dns_records_json` should prefer
	// [`Self::seed_awaiting_manual_dns_json`], which takes it verbatim.
	SeedAwaitingManualDns(nestUrl string, handle string, dnsRecords []DnsRecordPlain, claimCode string)
	// Relaunch hydration straight from the awaiting-DNS slot, taking the slot's
	// opaque `dns_records_json` verbatim.
	//
	// The companion to [`Self::awaiting_dns_records_json`]: together they keep
	// the record list **opaque to the client on both ends**, so a non-Rust
	// client never builds or parses that JSON. That is what stops the bindings'
	// camelCase (`recordType`) from silently round-tripping through serde's
	// snake_case (`record_type`) to an *empty* list — a failure that would only
	// surface after a relaunch, as an "Almost ready" page listing no records.
	//
	// A corrupt slot degrades to an empty record list rather than a panic: the
	// user still reaches the surface (and can re-run the DNS step) instead of
	// crashing on launch.
	SeedAwaitingManualDnsJson(nestUrl string, handle string, dnsRecordsJson string, claimCode string)
	// Relaunch hydration straight from the awaiting-DNS slot, taking the
	// **whole record** as the launch flow read it
	// (`fauna_launch_machine::AwaitingDnsRecord`) — the door every app's
	// relaunch glue should walk through, so a field the slot grows (the
	// reach address, the box's built-with identity) reaches the machine with
	// no per-app change at all. The two field-taking forms above and below
	// delegate here.
	//
	// Beyond what they seed, this re-holds the identity the box was built
	// with (`record.nest_actor_id`, `security.md` § Transport trust — the
	// *Client-provisioned box* row's "no TOFU window" holds across a
	// relaunch only because the slot carries the root) as the first-contact
	// root for the identity URL's host, and retains the row as the box this
	// machine is mid-way through provisioning, so a "start over" onto the
	// same domain after the relaunch resumes it rather than minting a fresh
	// identity of it (§ 6 *The pending-provision slot*).
	SeedAwaitingManualDnsRecord(record fauna_launch_machine.AwaitingDnsRecord)
	// Pre-seed the wizard with an identity loaded from the client's
	// long-term store at app launch. Skips identity-creation/import and
	// places the wizard at HandleEntry. No validation — the caller is
	// responsible for providing a valid 64-char hex secret it owns.
	//
	// `identity_origin` is left `None` (not `Imported`) so that pressing
	// Back from `HandleEntry` routes to `IdentityChoice` — the seeded
	// user never visited `IdentityImport`/`IdentityCreated` in this
	// session, so routing them there would show an empty paste form they
	// can't act on. From `IdentityChoice` they can re-pick if they want
	// to overwrite the seeded identity.
	SeedIdentity(secret string)
	// `launch-recover-button` on `launch_retry` (the surviving-device entry).
	// The client already holds its identity, so the glue passes it here; this
	// seeds it and drops straight into `NestRecovery` (the synced
	// `fauna.state.deployment-seeds` map is guaranteed local). Mirrors `seed_identity`, but for the
	// recovery branch. No validation — the caller owns a valid 64-hex secret.
	SeedIdentityForRecovery(secret string)
	SeedPendingInvite(nestUrl string, handle string, requestId string, statusJson string)
	SelectDnsProvider(id string)
	// User picked Public or Private on the `nat_mode_choice` page
	// (`public-nat-mode-radio` / `private-nat-mode-radio`). Sets
	// `selected_mode` and returns the snapshot to `Choosing` — including from
	// `Error` (re-picking recovers); submit stays enabled.
	// Per `docs/goal/behavior/onboarding.md` § 3b-bis.
	SelectNatMode(mode fauna_core.NodeMode)
	// Select a custodied box on `nest_recovery` (`recover-box-item`). Records
	// the box's `nest_actor_id` (hex); the method buttons stay disabled until a
	// box is selected. The raw seed is resolved in Rust at re-provision time
	// (`deployment_seed_for`), never surfaced here.
	SelectRecoveryBox(nestActorId string)
	SelectVpsLocation(id string)
	SelectVpsProvider(id string)
	SelectVpsServerType(id string)
	// Whether the currently-selected DNS registrar requires per-
	// registration WHOIS contact info (Gandi today; Porkbun = false).
	// Drives the `dns-contact-form` visibility per the target-state
	// doc's §4. Returns `false` when no provider is selected, when
	// the provider has no `registrar` capability, or when no creds
	// have been entered yet — the form only appears once a registrar
	// has been chosen and (post-`verify_dns`) the wizard has
	// confirmed the provider's contact requirements.
	SelectedRegistrarRequiresContact() bool
	// Set (or clear) the age claim the next admission carries
	// (`family-safety.md` § The account age band). The mobile apps call this
	// with their store age signal — attested when the platform attestation
	// round succeeded, declared-only otherwise; every other app never calls
	// it. Both `submit_invite_request` and `register` carry it through
	// [`Self::age_claim_to_send`], which strips an attestation the addressed
	// nest did not say it can check.
	SetAgeClaim(claim *AgeClaimPlain)
	// Sets the WHOIS contact for the buy-domain path. Called by the per-
	// client UI's contact form on submit. Replaces any contact previously
	// prefilled by `verify_dns` via `Registrar::fetch_default_contact()`.
	SetContact(contact fauna_provisioning.ContactInfo)
	SetControlCheckbox(checked bool)
	SetCurrentHandle(h string)
	SetDnsCred(fieldId string, value string)
	// Put a client-resolved message on the shared `error-message` channel.
	//
	// The counterpart of [`Self::begin_import_identity_with_reason`]'s
	// `reason`, without the transition: the machine holds no string table, so
	// a surface whose outcome is a typed value (today
	// [`RecoveryEntryOutcome`]) resolves the `onboarding.*` key its client-side
	// string table owns and stores the result here, where every app's
	// `error-message` already reads from. Keeping it on the machine rather
	// than in per-app view state is what makes the message survive the
	// observer tick that fires with it (the same reasoning
	// `begin_import_identity_with_reason` documents).
	SetErrorMessage(message string)
	// Apply a **nest hint** to the handle-entry page: pre-fill the handle's
	// domain part with `raw` when it classifies as a nest target, and report
	// whether it did (`onboarding.md` § 2 Handle entry → *Nest hint*).
	//
	// On web the hint is the `nest` query parameter a nest's central-origin
	// redirect carries (`web-content-hosting.md` § The nest-served `/app/`
	// and the central origin); a native deep link can hand the same raw value
	// here later. Parsing and the drop rule live HERE so every app applies the
	// same rule and none reads the raw hint into its own state:
	//
	// - The hint must name exactly one `host[:port]` authority
	// (`fauna_core::web::is_domain_authority_syntax` — no userinfo, path,
	// query or whitespace) before the shared probe classifier
	// (`resolve_handle_domain`) is trusted with it: that classifier is a
	// negative test and would call `evil.example/x?y` "public".
	// - A hint that does not classify is **dropped silently** — the field is
	// left as it was and `false` comes back; never an error message.
	// - It is a prefill and nothing more: any local part already typed is
	// kept, the step does not change, and the check, probe and trust
	// ceremony run exactly as for a typed domain. App-launch routing runs
	// before onboarding and never consults the hint, so a signed-in app
	// never reaches this call.
	SetNestHint(raw string) bool
	SetNestUrl(url string)
	SetPhase(phase HandleCheckPhase, msgKey string, args map[string]string)
	// Set the `vps-config-mail-mode-toggle`: whether the box provisions the mail
	// subsystem. `true` = a mail box (clamd/rspamd scanner sidecars + mail ports
	// `{25,465,587,993}`; needs a `mem_gb ≥ 2`
	// plan); `false` = a social-only box (lean nest+watchtower compose, viable on
	// the 1 GB tier — but it cannot later enable mail without a VPS resize). The
	// decision must live here at `vps_config`, not the post-claim §3b
	// enable-email checkbox, because cloud-init must know mail-intent (and the
	// RAM, picked on this same page) *before* the box boots. Drives
	// `CloudInitParams::enable_mail` via `run_provisioning_inner`. Defaults from
	// [`Self::provision_mail_mode_enabled`] (the handle's real-domain default)
	// until the user toggles it. Per `docs/goal/behavior/onboarding.md` §5.
	SetProvisionMailMode(enabled bool)
	// Select a `vps-config-update-channel-row`: which builds the box's
	// automatic updater follows. Decided at `vps_config`, like the mail mode,
	// because the channel's image tag is written into the box's cloud-init.
	// Drives `CloudInitParams::image_tag` via `run_provisioning_inner`. Per
	// `docs/goal/behavior/onboarding-provisioning.md` §5.
	SetProvisionUpdateChannel(channel fauna_provisioning.UpdateChannel)
	// Push the custodied box list into the machine for `nest_recovery` to
	// render (`recover-box-item`). The per-app glue fetches it from a
	// reachable nest via the shared `deploymentSeeds()` getter (owner-secret
	// read of `fauna.state.deployment-seeds`) and hands the resulting `nest_actor_id` (hex) list
	// here — held on the machine so every app renders one uniform list
	// (like `verify_vps` populating `vps.server_types`). Only public
	// `nest_actor_id`s cross; the seed stays in custody. A selection that is
	// no longer in the new list is cleared.
	SetRecoveryBoxes(boxes []string)
	// App capability declaration: this app has the `recovery_kit` onboarding
	// screen built (`onboarding.md` § 1 Identity). Call once at wizard
	// construction; the machine then routes `confirm_generated_identity`
	// through the kit screen. Apps that never call it keep the pre-existing
	// straight-to-`HandleEntry` flow — the batched-trickle-down parity gap.
	SetRendersRecoveryKit(renders bool)
	// App capability declaration: this app has the `trust_prompt` onboarding
	// screen built (`onboarding.md` § 3b-ter). Call once at wizard
	// construction; the machine then routes every *arrival* exit through the
	// one-tap trust offer — both NAT-page exits on the claim path, and both
	// join routes (invite redemption, approved request). Apps that never call
	// it keep the pre-existing straight-to-`Done` flow — the
	// batched-trickle-down parity gap, exactly as
	// [`Self::set_renders_recovery_kit`] works.
	SetRendersTrustPrompt(renders bool)
	// Re-root the re-provision drive's custody read at a sandboxed phone
	// shell's account-store container — the same container the shell hands the
	// account runtime (`fauna_client_account_runtime::SandboxedStoreContainer`),
	// so the local read opens the store the runtime writes. `None` restores the
	// per-OS platform root, which every desktop host and web use without ever
	// calling this.
	SetStoreContainerDir(storeContainerDir *string)
	SetVpsCred(fieldId string, value string)
	// Whether the dns_config page should render the WHOIS contact form. True
	// only when the selected registrar requires a contact AND the domain is
	// unregistered-but-buyable through it (`provider_status()` ==
	// `UnregisteredBuyable`, i.e. the buy-domain path actually runs). Clients
	// gate the contact form on this instead of re-combining
	// `selected_registrar_requires_contact()` with `provider_status()`.
	ShouldShowContactForm() bool
	// Whether the dns_config page should show the "no supported registrar
	// carries .{tld}" message. True when the user must buy the domain
	// (`buy_domain`) AND the handle-check probe found the domain available
	// but not buyable via any supported registrar
	// (`HandleCheckOutcome::DomainAvailable { buyable_via_provider: false }`).
	//
	// This is a TLD property knowable from the handle check alone: it does
	// NOT require the user to first select + verify a provider, and it does
	// not depend on *which* provider is selected. (Re-deriving it from
	// `provider_status() == UnregisteredNotBuyable` — as iOS/web/android did
	// before this getter — is both late (that status is `NotReady` until a
	// provider is selected + verified) and provider-specific (it would
	// mislabel a TLD that some registrar carries but the *selected* one does
	// not).) The message text is about the TLD, so the handle-check semantic
	// is the canonical one (`onboarding.md` § 4). Clients gate
	// `dns-no-provider-message` on this getter instead of re-deriving it
	// (priority #2; mirrors `should_show_contact_form` /
	// `should_show_registrar_notes`).
	ShouldShowNoProviderMessage() bool
	// Whether the registrar-specific notes blurb (e.g. Porkbun's
	// account-contact requirement) should be shown: only on the buy-domain
	// path, and only when the selected provider declares a
	// `registrar_notes_key`.
	ShouldShowRegistrarNotes() bool
	// `recovery-kit-skip-button`: decline the kit. One click, never blocks
	// onboarding; the minted root is dropped (zeroized) so nothing registers
	// at handoff, and Settings' never-created warning tells the truth.
	SkipRecoveryKit()
	// `trust-box-skip-button`: decline. "Declining leaves everything as
	// today" (§ 3b-ter) — nothing is latched, nothing is minted, and the
	// wizard concludes identically.
	SkipTrustPrompt() OnboardingStep
	StartHandleCheck(handle string)
	// Spawns the four-step provisioning orchestrator on the appropriate
	// runtime (`tokio::spawn` on native, `wasm_bindgen_futures::spawn_local`
	// on web) and returns immediately. All inputs are read from the
	// machine's existing state (handle, DNS provider/creds, VPS
	// provider/creds, contact, set-up-later flag). Observer ticks drive
	// re-render; the client reads `provisioning_snapshot()` on each tick.
	//
	// Idempotent: safe to call after a partial prior run — the
	// orchestrator's pre-flight checks short-circuit completed work.
	StartProvisioning()
	Step() OnboardingStep
	SubmitHandleCheckContinue() OnboardingStep
	// Wire-up for the `nat-mode-confirm-button`. Commits the admin's NAT-axis
	// choice via the **mutable** `fauna.setup.nat_mode` kind (pre-identity,
	// Ed25519-signed over `mode_wire_str ‖ "\n" ‖ actor_id_hex ‖ "\n" ‖
	// timestamp_decimal` — the same signed envelope as the storage-mode
	// commit; any valid admin-signed set upserts the `nest_nat_mode` row, no
	// conflict reply). On success the wizard exits: state → `Done`,
	// `wizard_outcome() == LoggedIn`, returns `OnboardingStep::Done`. On a
	// 4xx-class reject the snapshot moves to `Error { transient: false }`; on
	// transport/internal failure `Error { transient: true }` — submit stays
	// enabled either way (resubmit allowed). Per
	// `docs/goal/behavior/onboarding.md` § 3b-bis.
	SubmitNatModeChoice() OnboardingStep
	// `recovery-entry-submit-button` — the phrase-only identity restore
	// (`onboarding.md` § 1 Identity; ceremony
	// `identity-succession.md` § Seed escrow → *Restore path*).
	//
	// `kit_input` is `recovery-entry-phrase-field` verbatim (the
	// `fauna://recovery` URI or a bare 64-hex code). The account comes from
	// [`Self::current_handle`] — the wizard's one account field, which the
	// client fills from `recovery-entry-account-field` when the user typed
	// one; when it is empty the kit payload's own `handle=` supplies it, and
	// this method writes that back so `handle_entry` pre-fills exactly as an
	// `identity_import` QR's handle does.
	//
	// On success the seed is restored and the wizard lands on `HandleEntry` —
	// deliberately the same landing `confirm_imported_identity` produces: the
	// seed *is* recovered, so everything downstream is an import.
	//
	// Returns the outcome rather than a message because the machine holds no
	// string table; the client resolves the i18n key
	// (`onboarding.recovery_entry.*`) and renders it on `error-message`, the
	// same division the handle-check's `LocalizedText` snapshots use.
	// [`RecoveryEntryOutcome::Superseded`] is the one variant that does not
	// stay on the page: the client routes it to
	// [`Self::begin_import_identity_with_reason`], uniform with the launch
	// flow's superseded refusal.
	SubmitRecoveryEntry(kitInput string) RecoveryEntryOutcome
	// Consume the pending RecoveryKey root at the wizard's signed-in handoff —
	// the one point custody permits registration (the root is never
	// persisted, so it cannot survive to a later session). Returns `None` if
	// the kit was skipped, already taken, or never offered.
	TakePendingRecoverySecret() *fauna_core.SecretString
	// Consume the `trust_prompt` answer at the wizard's signed-in handoff —
	// the one point the client holds an authenticated session and the nest's
	// content-processor roster, i.e. the only place
	// `LinkedNestsAction::MintDefaultSet` can run. Consume-once (mirrors
	// [`Self::take_pending_recovery_secret`]), so a handoff that runs twice
	// mints once. `false` when the user skipped, never asked, or already
	// handed off.
	TakeTrustPromptGranted() bool
	ToggleBuyDomain(on bool)
	ToggleSameProviderForVps(on bool)
	VerifyDns() error
	VerifyOobInviteCode(code string)
	VerifyVps() error
	// Fields visible on the dns_config form. Filters by `FieldMeta::kinds`:
	// always include kinds containing Dns; additionally include Vps-kinded
	// fields when same_provider_for_vps is on AND the provider has Vps cap,
	// and Registrar-kinded fields when buy_domain is on AND the provider has
	// Registrar cap (e.g. cloudflare's `account-id`, `kinds: [registrar]` —
	// without this arm a registrar-only-kinded field never renders, so its
	// credential is never collected and the registrar API call it feeds
	// fails downstream instead of at the form).
	VisibleDnsFields() []FieldMetaPlain
	// Fields visible on the vps_config form. Filters the selected VPS
	// provider's fields by `FieldMeta::kinds` containing `Capability::Vps`.
	// Mirrors `visible_dns_fields()` so per-app view code is a one-liner
	// (`_m.VisibleVpsFields()`) instead of a duplicated client-side filter.
	VisibleVpsFields() []FieldMetaPlain
	VpsConfig() VpsConfigState
	// Why `vps-config-continue-button` is dead — `None` when it is live, else
	// the one-line explainer naming **the act that revives it**.
	//
	// **This exists because a disabled control owes the user a reason**
	// (`ui/README.md` § Copy comprehensibility rule 5; `apps/tui.md`
	// § Rendering → *Control vocabulary* rule 3 restates it for the terminal,
	// where DIM is the only other signal). The vps_config page shipped with
	// **no** explanatory surface at all: a first-time user landed on four
	// provider names, none marked, and a dead Continue, with nothing on screen
	// saying what to do — the exact defect [`Self::dns_status_text_key`] was
	// split in two to fix one page earlier in the same wizard, found here by
	// the walk's wizard driver (`walk.rs`, 2026-08-05).
	//
	// Paired with [`Self::can_continue_vps`] over one private shortfall, so
	// the verdict and its explanation cannot disagree — the
	// `dns_provider_eligible` / `dns_provider_ineligible_reason` template. An
	// `Option` rather than a "" key on purpose: a blank-string status is
	// precisely the bug that left `dns-status-text` painting an empty line in
	// its gating state, and `None` makes that unrepresentable.
	//
	// Four shortfalls, four messages, not one generic line (rule 5 Q2 — the
	// user's next act differs in each: pick, verify, choose where, choose how
	// big).
	VpsContinueBlockedReason() *fauna_core.LocalizedText
	// Whether the deployment's **files (WebDAV)** subsystem should be enabled
	// at claim. Machine-derived sibling of [`Self::carddav_enable_requested`] —
	// same two-axis derivation (handle locality AND NAT axis), no checkbox
	// (`onboarding.md` § 3b).
	//
	// The per-app launch glue calls `MailAdminClient::set_webdav_enabled(true)`
	// on it at `LoggedIn`. Per `docs/goal/behavior/webdav-server.md`
	// § Independent enablement.
	WebdavEnableRequested() bool
	WizardOutcome() *WizardOutcome
	// Wire-up for the `claim-code-submit-button`. POSTs the user's
	// one-time claim code + Ed25519-signed auth payload to
	// `POST /api/v1/claim-admin`. On 2xx, the wizard transitions to
	// `Done` with `wizard_outcome() == LoggedIn { nest_url, handle }`;
	// on 4xx the wizard stays on `claim_code` with the snapshot moved
	// to `Invalid { reason }`; on 5xx / network the snapshot moves to
	// `Error { transient: true, ... }`. Per
	// `docs/goal/behavior/onboarding.md` §3a.
	WizardSubmitClaimCode(code string) OnboardingStep
	WizardSubmitInviteRequest() OnboardingStep
}

// The single shared onboarding wizard state machine. Each app holds
// `Arc<OnboardingMachine>`, observes via the registered `OnboardingObserver`,
// and mutates via the methods exposed below.
//
// Wizard state is in-memory only — there is no persistence layer here.
// Identity-confirmation methods return the secret hex so the per-app
// glue can write it to its long-term store immediately, and `seed_identity`
// lets app-launch code pre-populate the wizard at HandleEntry from a
// previously-stored secret.
//
// Uses `std::sync::Mutex` (not `tokio::sync::Mutex`) so getters and sync
// mutations work from any thread context — including UI threads on native
// apps and `#[tokio::test]` runtimes. Async methods take a snapshot under
// the lock, drop the lock, do IO, then re-acquire to apply changes; the lock
// is never held across an `await`.
type OnboardingMachine struct {
	ffiObject FfiObject
}

// Creates a new machine. Always starts fresh at `IdentityChoice`;
// callers that want to resume a prior identity should construct, then
// call `seed_identity(secret)` to jump to `HandleEntry`.
//
// This is the **only** constructor a production artifact carries, and it
// takes no override map — every native app already passed `None` there, so
// the parameter was pure automation surface riding the exported UniFFI
// signature (`testing.md` convention 15). E2E builds reach the map through
// the `test-helpers`-gated pair further down this file: a construction-time
// twin of this constructor (what web needs, since it rebuilds the machine
// on reload) and a runtime setter driven by the bridge (what the
// long-lived native machines need).
//
// Deliberately phrased without naming either gated symbol: UniFFI copies
// doc comments into the generated metadata, so a name written here lands
// in every release artifact's `strings` and muddies the very grep that
// convention 15 uses as its absence proof.
func NewOnboardingMachine(observer OnboardingObserver) *OnboardingMachine {
	return FfiConverterOnboardingMachineINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_fauna_onboarding_machine_fn_constructor_onboardingmachine_new(FfiConverterOnboardingObserverINSTANCE.Lower(observer), _uniffiStatus)
	}))
}

// [`Self::new`] plus the pending-provision slot's writer — **the form every
// production app builds the wizard with** (`docs/goal/behavior/onboarding.md`
// § 6 *The pending-provision slot*).
//
// A second constructor rather than a widened `new`, following
// [`Self::new_with_provider_base_urls`]: `new` has ~30 in-repo test call
// sites that want the plain form and no slot, and widening it would make
// every one of them carry a `None` that says nothing.
//
// The cost of a *second* constructor is that an app which forgets to switch
// loses the crash resume silently — which is exactly the failure class this
// row exists to remove — so the six production sites are pinned by
// `provision_slot_is_wired_on_every_production_app`, and that test fails on
// the machine of whoever drops one rather than in a user's crashed wizard.
func OnboardingMachineNewWithPersistence(observer OnboardingObserver, persistence fauna_launch_machine.PendingProvisionStore) *OnboardingMachine {
	return FfiConverterOnboardingMachineINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_fauna_onboarding_machine_fn_constructor_onboardingmachine_new_with_persistence(FfiConverterOnboardingObserverINSTANCE.Lower(observer), func(value fauna_launch_machine.PendingProvisionStore) C.uint64_t {
			return C.uint64_t(fauna_launch_machine.LowerToExternalPendingProvisionStore(value))
		}(persistence), _uniffiStatus)
	}))
}

// The "Almost ready" surface's explicit exit — **"Use a different nest"**
// (`onboarding-provisioning.md` § "Almost ready" surface → *Exit*): the way
// out for a box that will never answer (a `create_server` that failed after
// the slot was written, a box deleted at the provider), without which a
// resumed launch is pinned on a waiting page for ever.
//
// Clears the awaiting slot of the identity being onboarded (through the
// injected [`PendingProvisionStore`], addressed by that identity's secret —
// on an append run the active account is a different one) and lands the wizard
// at `HandleEntry` **holding the same identity**: the user is choosing a
// different nest, not a different self. The landing is `launch_retry`'s
// fallthrough (`seed_identity`) with the surface's state — outcome, snapshot,
// provisioning run, reach override — cleared behind it; the machine's
// in-memory pending-provision row survives, as it does every `reset()`, so
// choosing the SAME domain again resumes the box rather than minting a
// second one (§ 6 *The pending-provision slot*).
//
// The slot is cleared BEFORE the state moves: a crash between the two
// relaunches onto the surface with the exit still on it, which is a
// recoverable place, where the other order would land a slot-less identity
// on a wizard the user never asked for.
//
// No-op unless `wizard_outcome()` is `AwaitingManualDns` — a stray call must
// not retire a resumable box's slot — and while a claim is in flight
// ([`Self::awaiting_dns_fallthrough_enabled`]), which would race the claim's
// own nest-binding write.
func (_self *OnboardingMachine) AbandonAwaitingManualDns() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_abandon_awaiting_manual_dns(
			_pointer, _uniffiStatus)
		return false
	})
}

// The age claim the glue set, if any — verbatim, attestation included
// (the iOS round reads it to decide a re-mint). What actually rides the
// wire is [`Self::age_claim_to_send`].
func (_self *OnboardingMachine) AgeClaim() *AgeClaimPlain {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalAgeClaimPlainINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_age_claim(
				_pointer, _uniffiStatus),
		}
	}))
}

// SHA-256 of [`Self::age_claim_message`] — the ONE value both platform
// attestations bind: iOS hands it to App Attest as `clientDataHash`;
// android passes it base64url-unpadded as the Play Integrity classic
// request's nonce. A digest on purpose: Apple's and Google's logs see
// 32 opaque bytes, never the band or the actor id in the pre-image. The
// nest recomputes exactly this on both arms.
func (_self *OnboardingMachine) AgeClaimDigest(nonceHex string, band string, applicationId string) ([]byte, error) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*OnboardingError](FfiConverterOnboardingError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_age_claim_digest(
				_pointer, FfiConverterStringINSTANCE.Lower(nonceHex), FfiConverterStringINSTANCE.Lower(band), FfiConverterStringINSTANCE.Lower(applicationId), _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue []byte
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterBytesINSTANCE.Lift(_uniffiRV), nil
	}
}

// The exact bytes the platform attestation must commit to —
// `fauna_protocol::age::age_claim_signed_message(nonce, band,
// application_id, actor_id)` for the wizard's current identity. What
// the platforms actually consume is their SHA-256,
// [`Self::age_claim_digest`]; this is the pre-image, for transparency
// and tests. One definition, shared with the nest's verifier.
func (_self *OnboardingMachine) AgeClaimMessage(nonceHex string, band string, applicationId string) ([]byte, error) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*OnboardingError](FfiConverterOnboardingError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_age_claim_message(
				_pointer, FfiConverterStringINSTANCE.Lower(nonceHex), FfiConverterStringINSTANCE.Lower(band), FfiConverterStringINSTANCE.Lower(applicationId), _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue []byte
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterBytesINSTANCE.Lift(_uniffiRV), nil
	}
}

// Whether the "Almost ready" surface's "Copy all" button has anything to
// copy. False in the records-less mode — the resumed standard path, whose
// DNS was ours to write — where [`Self::awaiting_dns_records_text`] is
// empty and a click would copy nothing.
//
// A getter rather than a rule each app applies to `dns_records`, the same
// division `awaiting_dns_records_text` above already makes for the text
// itself: the app asks, the machine decides.
// See [`crate::snapshots::awaiting_manual_dns::copy_all_enabled`].
func (_self *OnboardingMachine) AwaitingDnsCopyEnabled() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_awaiting_dns_copy_enabled(
			_pointer, _uniffiStatus)
	}))
}

// Whether the "Almost ready" surface's exit ("Use a different nest") may be
// taken now — off only while a claim is in flight. The app asks, the machine
// decides, exactly as for [`Self::awaiting_dns_copy_enabled`].
// See [`crate::snapshots::awaiting_manual_dns::fallthrough_enabled`].
func (_self *OnboardingMachine) AwaitingDnsFallthroughEnabled() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_awaiting_dns_fallthrough_enabled(
			_pointer, _uniffiStatus)
	}))
}

// The deferred-DNS records as the exact JSON the awaiting-DNS slot carries:
// `serde_json::to_string(&dns_records)` over the wizard's
// `Vec<DnsRecordPlain>` — the `dns_records_json` field of
// `fauna_launch_machine::AwaitingDnsRecord`.
//
// **Exists so no non-Rust client ever hand-rolls that JSON.** The seeder
// (`seed_awaiting_manual_dns`) parses it back with serde, which expects
// serde's field names (`record_type`, …) — but the UniFFI and WASM bindings
// expose the record as `recordType`. A client-side `Gson`/`JSON.stringify`
// of the *bound* type therefore produces JSON that deserializes to an empty
// list, silently losing the records the user still has to add at their
// registrar — a failure that only shows up after a relaunch. Rust clients
// call `serde_json::to_string` directly and get byte-identical output.
func (_self *OnboardingMachine) AwaitingDnsRecordsJson() string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_awaiting_dns_records_json(
				_pointer, _uniffiStatus),
		}
	}))
}

// The records the user must add at their registrar, formatted for display —
// one line per record. Every app renders the "Almost ready" records from
// this, and copies *this* to the clipboard, so the label and the copy button
// can never disagree and all seven apps give the same instruction.
// See [`crate::snapshots::awaiting_manual_dns::format_dns_records`].
func (_self *OnboardingMachine) AwaitingDnsRecordsText() string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_awaiting_dns_records_text(
				_pointer, _uniffiStatus),
		}
	}))
}

// Snapshot for the post-provisioning "Almost ready" surface. Pure read;
// cheap clone. The client renders this on every observer tick while
// `wizard_outcome()` is `AwaitingManualDns`. Per
// `docs/goal/behavior/onboarding.md` § "Wizard exit handling".
func (_self *OnboardingMachine) AwaitingManualDnsSnapshot() AwaitingManualDnsSnapshot {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterAwaitingManualDnsSnapshotINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_awaiting_manual_dns_snapshot(
				_pointer, _uniffiStatus),
		}
	}))
}

// Stage-aware Back button. The wizard's nav graph is small enough to
// hardcode here so each app doesn't replicate the routing.
func (_self *OnboardingMachine) Back() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_back(
			_pointer, _uniffiStatus)
		return false
	})
}

// Enter `identity_created`. Entry **mints, and does not commit.**
//
// The origin is deliberately *not* written here. [`State::canonical_secret`]
// reads it as *"the screen the user committed on"* (`onboarding.md`
// § 1 Identity), and this screen's entry fills its own slot — so an
// entry-time write would make mere curiosity outrank an identity the user
// already pasted and confirmed: import the real key, tap "create" to look
// around, go Back, and every authenticating call signs as the throwaway
// while the wizard terminal persists it (§ Long-term store contract).
// [`Self::begin_import_identity`] *can* afford the entry-time write because
// it leaves its slot empty, so "origin says `Imported`, slot is empty" is a
// legible back-out state the fallback arm covers; a minted-on-entry slot
// has no such tell. The commit point is [`Self::confirm_generated_identity`].
//
// The mint is guarded for the same reason the recovery-kit root is
// (see [`Self::confirm_generated_identity`]): re-entry after
// back-navigation must re-show the SAME key, or a key the user was just
// told to write down is silently invalidated.
func (_self *OnboardingMachine) BeginCreateIdentity() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_begin_create_identity(
			_pointer, _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) BeginImportIdentity() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_begin_import_identity(
			_pointer, _uniffiStatus)
		return false
	})
}

// [`Self::begin_import_identity`], carrying the reason the user was *sent*
// there — for arrivals the user did not ask for.
//
// The launch flow's `superseded` refusal is the case that needs it
// (`identity-succession.md` § Propagation → *Own device fleet*: the client
// "surfaces 'this identity was succeeded — import the new identity'"): the
// affordance IS the import screen, so the only thing distinguishing it from
// a user who chose to import is the explanation on that page's existing
// `error-message`. `recovery_entry`'s own superseded refusal
// (`onboarding.md` § 1 Identity) routes the same way.
//
// **Why the reason lives in machine state rather than at the call site.**
// The per-app onboarding views mirror `error_message()` reactively on every
// observer tick (linux's `handle_change` re-reads it into the GTK label each
// time), so a reason written straight to a widget is erased by the next tick
// — including the tick this very transition fires. Setting both fields under
// one [`Self::mutate`] also makes the pair atomic: no observer ever sees the
// import step without the explanation that justifies it.
func (_self *OnboardingMachine) BeginImportIdentityWithReason(reason string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_begin_import_identity_with_reason(
			_pointer, FfiConverterStringINSTANCE.Lower(reason), _uniffiStatus)
		return false
	})
}

// `recover-lost-box-button` on `identity_choice` (the fresh re-onboarded
// client entry). Marks recovery intent and routes to `identity_import`
// first — the admin's identity must be loaded to read the
// `fauna.state.deployment-seeds` map) — then `confirm_imported_identity` lands on
// `NestRecovery` (the `condition: "recovery-intent"` transition).
func (_self *OnboardingMachine) BeginRecoverLostBox() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_begin_recover_lost_box(
			_pointer, _uniffiStatus)
		return false
	})
}

// `restore-from-recovery-kit-button` on `identity_choice` — the
// phrase-only IDENTITY restore (`onboarding.md` § 1 Identity). Routes to
// the `recovery_entry` screen. Distinct from `begin_recover_lost_box`,
// which starts the total-box-loss NEST recovery branch.
func (_self *OnboardingMachine) BeginRecoveryEntry() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_begin_recovery_entry(
			_pointer, _uniffiStatus)
		return false
	})
}

// The `nest_provisioning` page's top-region price summary — up to two
// line items: the domain's one-time registration price (only when the
// wizard is buying a new domain) and the selected VPS's monthly price
// (always present — `vps_config`'s Continue requires a selection).
// Both prices were already shown and, for the domain, explicitly
// agreed to earlier in the wizard (`dns-tld-price-display` /
// `dns-price-confirm-checkbox` on `dns_config`, the server-type radio
// options on `vps_config`); this is a pre-commit recap before
// `provisioning-start-button`, not a new price source. Pure
// computation over already-in-state DNS/VPS data — no IO.
// `docs/goal/behavior/onboarding.md` §6.
func (_self *OnboardingMachine) BillOfMaterials() []BillOfMaterialsItem {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterSequenceBillOfMaterialsItemINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_bill_of_materials(
				_pointer, _uniffiStatus),
		}
	}))
}

// `bill_of_materials()`'s non-recurring (domain) item, pre-folded into
// one [`LocalizedText`] ready for `resolve_nested` — `None` when
// nothing is chargeable. `{label}` carries the step's own i18n key
// (never its own args — `step_label` is always a bare key — so passing
// it as a `resolve_nested` arg is safe) and `{price}`/`{renewal}` the
// shared [`crate::helpers::format_price`] output. Picks the
// `bom_line_domain` key over plain `bom_line` when the registrar
// quoted a renewal price (`onboarding.md` § 6: disclosed before the
// charge). Lifted out of per-app derivation — linux and tui carried
// mirrored, independently-drifting copies of this exact branch.
func (_self *OnboardingMachine) BomDomainLine() *fauna_core.LocalizedText {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalLocalizedTextINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_bom_domain_line(
				_pointer, _uniffiStatus),
		}
	}))
}

// `bill_of_materials()`'s recurring (VPS) item, pre-folded into one
// [`LocalizedText`] ready for `resolve_nested` — `None` when nothing is
// selected yet. Always the `bom_line_recurring` key: VPS pricing has no
// first-year/renewal split. See [`Self::bom_domain_line`] for the
// `{label}`-is-a-key rationale.
func (_self *OnboardingMachine) BomVpsLine() *fauna_core.LocalizedText {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalLocalizedTextINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_bom_vps_line(
				_pointer, _uniffiStatus),
		}
	}))
}

// Whether the deployment's **calendar (CalDAV)** subsystem should be
// enabled at claim. Machine-derived sibling of
// [`Self::email_enable_requested`] — same two-axis derivation
// (handle locality AND NAT axis), no checkbox (`onboarding.md` § 3b; the
// DAV enables also mint the shared MSEK when first, so the home-relay
// divergence argument covers them too).
//
// The per-app launch glue calls `MailAdminClient::set_caldav_enabled(true)`
// on it at `LoggedIn`. The subsystems stay **separately gated** (CalDAV needs
// only the HTTPS surface — no MX/DKIM — so it can be on where email is off);
// they merely share this default today. Per
// `docs/goal/behavior/caldav-server.md` § Independent enablement.
func (_self *OnboardingMachine) CaldavEnableRequested() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_caldav_enable_requested(
			_pointer, _uniffiStatus)
	}))
}

func (_self *OnboardingMachine) CanContinueDns() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_can_continue_dns(
			_pointer, _uniffiStatus)
	}))
}

// Wizard-exit Continue button enables — provisioning Succeeded.
func (_self *OnboardingMachine) CanContinueProvisioning() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_can_continue_provisioning(
			_pointer, _uniffiStatus)
	}))
}

func (_self *OnboardingMachine) CanContinueVps() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_can_continue_vps(
			_pointer, _uniffiStatus)
	}))
}

// Retry button is shown — the run Failed or was soft-Cancelled.
func (_self *OnboardingMachine) CanRetryProvisioning() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_can_retry_provisioning(
			_pointer, _uniffiStatus)
	}))
}

func (_self *OnboardingMachine) CanVerifyDns() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_can_verify_dns(
			_pointer, _uniffiStatus)
	}))
}

func (_self *OnboardingMachine) CanVerifyVps() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_can_verify_vps(
			_pointer, _uniffiStatus)
	}))
}

func (_self *OnboardingMachine) CancelHandleCheck() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_cancel_handle_check(
			_pointer, _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) CancelInviteOp() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_cancel_invite_op(
			_pointer, _uniffiStatus)
		return false
	})
}

// Sets the cancel flag. The running provisioning task observes it at
// the next step boundary or retry iteration. Soft-cancel only —
// already-created VPS/DNS resources stay; a subsequent retry picks
// them up via idempotency.
func (_self *OnboardingMachine) CancelProvisioning() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_cancel_provisioning(
			_pointer, _uniffiStatus)
		return false
	})
}

// The DNS-provider credential captured at the onboarding DNS step, in the
// shape the launched client's `DnsManagementMachine::PutCredentials`
// consumes. `None` unless a provider was selected, `verify_dns()`
// succeeded, and the admin did **not** choose "set up later" — i.e. there
// is a verified credential worth sealing.
//
// This is the onboarding→launch **hand-off channel** for the client-side
// DNS credential store (`docs/goal/behavior/dns-management.md` § Where the
// credential lives; `docs/goal/behavior/onboarding.md` § 4). The
// onboarding machine has no account-plane write capability and, in the
// fresh-provision path, no live nest "at the end of the DNS step"; so
// rather than sealing here it exposes the captured credential and the
// **launched client** seals it once authenticated, via the normal
// `PutCredentials` path — one store, one writer (the
// `DnsManagementMachine`), no second config-put path in onboarding. The
// per-app launch glue reads this at `wizard_outcome() == LoggedIn` and
// dispatches `PutCredentials { provider_id, fields, label }` through
// `build_dns_management_machine_with_credentials`. The machine re-runs
// `verify()` to (re)derive covered zones, so the captured zones are not
// re-surfaced here.
func (_self *OnboardingMachine) CapturedDnsCredential() *CapturedDnsCredential {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalCapturedDnsCredentialINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_captured_dns_credential(
				_pointer, _uniffiStatus),
		}
	}))
}

// Whether the deployment's **contacts (CardDAV)** subsystem should be
// enabled at claim. Machine-derived sibling of
// [`Self::caldav_enable_requested`] — same two-axis derivation (handle
// locality AND NAT axis), no checkbox (`onboarding.md` § 3b).
//
// The per-app launch glue calls `MailAdminClient::set_carddav_enabled(true)`
// on it at `LoggedIn` and, when neither email nor CalDAV minted the shared
// MSEK, provisions the CardDAV-only mailbox
// (`enable_carddav_mailbox_with_generated_password`). Per
// `docs/goal/behavior/carddav-server.md` § Independent enablement.
func (_self *OnboardingMachine) CarddavEnableRequested() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_carddav_enable_requested(
			_pointer, _uniffiStatus)
	}))
}

// The claim code a client wants the `ClaimCode` page input pre-filled with
// (the factory-reset re-onboard path), or `None` for the ordinary path
// where the human types it. See
// [`navigate_to_claim_code_for_known_nest_with_code`].
func (_self *OnboardingMachine) ClaimCodePrefill() *string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_claim_code_prefill(
				_pointer, _uniffiStatus),
		}
	}))
}

// Snapshot for the `claim_code` page. Pure read; cheap clone of a
// small enum + `LocalizedText`. Per
// `docs/goal/behavior/onboarding.md` §3a — clients render this on every
// observer tick.
func (_self *OnboardingMachine) ClaimCodeSnapshot() ClaimCodeSnapshot {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterClaimCodeSnapshotINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_claim_code_snapshot(
				_pointer, _uniffiStatus),
		}
	}))
}

func (_self *OnboardingMachine) ClearError() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_clear_error(
			_pointer, _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) Complete(outcome HandleCheckOutcome, msgKey string, args map[string]string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_complete(
			_pointer, FfiConverterHandleCheckOutcomeINSTANCE.Lower(outcome), FfiConverterStringINSTANCE.Lower(msgKey), FfiConverterMapStringStringINSTANCE.Lower(args), _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) CompleteProbeError(phase HandleCheckPhase, transient bool, cause string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_complete_probe_error(
			_pointer, FfiConverterHandleCheckPhaseINSTANCE.Lower(phase), FfiConverterBoolINSTANCE.Lower(transient), FfiConverterStringINSTANCE.Lower(cause), _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) ConfirmGeneratedIdentity() (string, error) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*OnboardingError](FfiConverterOnboardingError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_confirm_generated_identity(
				_pointer, _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue string
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterStringINSTANCE.Lift(_uniffiRV), nil
	}
}

func (_self *OnboardingMachine) ConfirmImportedIdentity(secret string) (string, error) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*OnboardingError](FfiConverterOnboardingError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_confirm_imported_identity(
				_pointer, FfiConverterStringINSTANCE.Lower(secret), _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue string
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterStringINSTANCE.Lift(_uniffiRV), nil
	}
}

// Records the user's explicit acceptance of the displayed registration
// price. Required before `start_provisioning()` will run the buy-domain
// path. Pair with `dns_status_text()` (or its i18n key variant) so the
// UI shows the user the exact price before they consent.
func (_self *OnboardingMachine) ConfirmPrice() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_confirm_price(
			_pointer, _uniffiStatus)
		return false
	})
}

// `recovery-kit-confirm-button`: the user says the phrase is saved.
// Advances to `HandleEntry`; the pending root is KEPT for the signed-in
// handoff, where the per-app glue takes it
// ([`Self::take_pending_recovery_secret`]) and runs registration + escrow.
func (_self *OnboardingMachine) ConfirmRecoveryKit() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_confirm_recovery_kit(
			_pointer, _uniffiStatus)
		return false
	})
}

// Sync transition. The actual buy / register happens in
// `start_provisioning()` so the user can still back out before
// committing money.
func (_self *OnboardingMachine) ContinueFromDns() error {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	_, _uniffiErr := rustCallWithError[*OnboardingError](FfiConverterOnboardingError{}, func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_continue_from_dns(
			_pointer, _uniffiStatus)
		return false
	})
	return _uniffiErr.AsError()
}

// Continue button on the `dns_post_instructions` page. Sets
// `wizard_outcome()` to `AwaitingManualDns { nest_url, dns_records,
// claim_code }` and returns `OnboardingStep::Done`. Caller is
// responsible for navigating to the "Almost ready" surface.
func (_self *OnboardingMachine) ContinueFromDnsPostInstructions() OnboardingStep {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOnboardingStepINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_continue_from_dns_post_instructions(
				_pointer, _uniffiStatus),
		}
	}))
}

// Bottom-row Continue on the `nest_provisioning` page. Advances only when
// the provisioning snapshot is `Succeeded`; otherwise returns the current
// step unchanged so the click is a no-op.
//
// On the deferred-DNS path it transitions to `DnsPostInstructions`. On the
// standard path it lands on **`NatModeChoice`** (§ 3b-bis), exactly as a
// claim-code submit does — this page never exits straight to
// `Done`/`LoggedIn` (`docs/goal/behavior/onboarding.md` § 6, ratified
// 2026-08-29). `Succeeded` there means the box is built *and claimed* (the
// `Online` claiming substep, `run_provisioning_claim`), so the § 3b-bis /
// § 3b-ter tail owns the exit from here on. The pre-ratification `LoggedIn` /
// `Done` exit signed the user in to a box **nobody had claimed** and then
// bounced them to a claim page asking for a code they never saw.
//
// The standard path additionally requires the claim to have actually
// completed. That is not belt-and-braces: the orchestrator marks the run
// `Succeeded` and notifies before returning, so an app that paints and takes
// a click inside the window before `set_claiming` reopens the step could
// otherwise Continue past an unclaimed box. The gate is the claim's own
// state, not a timing assumption.
func (_self *OnboardingMachine) ContinueFromProvisioning() OnboardingStep {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOnboardingStepINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_continue_from_provisioning(
				_pointer, _uniffiStatus),
		}
	}))
}

// VPS-stage Continue button (`vps-config-continue-button`). Validates
// the form (verified credentials, location chosen, server type
// chosen) and transitions the wizard to `NestProvisioning`. The
// orchestrator is kicked separately by the architecture-defined
// "Buy and set up" CTA on the `nest_provisioning` page, which calls
// `start_provisioning()` — that gives the user a final price-review
// gate before money is committed.
func (_self *OnboardingMachine) ContinueFromVps() error {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	_, err := uniffiRustCallAsync[*OnboardingError](
		FfiConverterOnboardingErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_continue_from_vps(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

	if err == nil {
		return nil
	}

	return err
}

func (_self *OnboardingMachine) CurrentHandle() string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_current_handle(
				_pointer, _uniffiStatus),
		}
	}))
}

// Wire-up for the `nat-mode-defer-button`: exit the `nat_mode_choice`
// page without committing. The seeded mode stays in effect server-side
// (already a working default — nothing is sent); the admin can set it
// later from the admin panel. The wizard exits exactly as a successful
// submit does: `wizard_outcome() == LoggedIn`, step → `Done`. Per
// `docs/goal/behavior/onboarding.md` § 3b-bis.
func (_self *OnboardingMachine) DeferNatModeChoice() OnboardingStep {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOnboardingStepINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_defer_nat_mode_choice(
				_pointer, _uniffiStatus),
		}
	}))
}

func (_self *OnboardingMachine) DnsConfig() DnsConfigState {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterDnsConfigStateINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_dns_config(
				_pointer, _uniffiStatus),
		}
	}))
}

// Renders the captured DNS records as the markdown table the
// `dns_post_instructions` page surfaces — same shape the orchestrator's
// `DeferredDnsResult.instructions_markdown` produces. Returns `None`
// when records or the provisioning result aren't populated yet
// (i.e. the deferred-DNS run hasn't finished). Clients that prefer to
// render their own table can read `dns_records()` and the snapshot's
// result directly.
func (_self *OnboardingMachine) DnsPostInstructions() *string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_dns_post_instructions(
				_pointer, _uniffiStatus),
		}
	}))
}

// Whether the DNS-provider button for `provider_id` should be selectable
// given the user's current DNS choices. A provider is *ineligible* when
// the user wants to buy a domain but it can't register
// (`buy_domain && !Registrar`), or wants one provider for both DNS and
// VPS but it has no VPS capability (`same_provider_for_vps && !Vps`).
// Unknown ids are never eligible. This is the queryable form of the
// deselect-on-toggle guards in `toggle_buy_domain` /
// `toggle_same_provider_for_vps`; all apps drive per-provider button
// sensitivity through it instead of re-deriving the capability rule.
func (_self *OnboardingMachine) DnsProviderEligible(providerId string) bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_dns_provider_eligible(
			_pointer, FfiConverterStringINSTANCE.Lower(providerId), _uniffiStatus)
	}))
}

// Why the `dns-provider-row[<id>]` control is not selectable — `None` when
// it is, else the i18n key of a one-line explainer naming the constraint
// that closed it *and the checkbox that re-opens it*.
//
// **This exists because a disabled control owes the user a reason**
// (`ui/README.md` § Copy comprehensibility rule 5 — the cross-app owner;
// `apps/tui.md` § Rendering → *Control vocabulary* rule 3 restates it for
// the terminal, where DIM is the only other signal). [`Self::
// dns_provider_eligible`] answers yes/no, which is enough to grey a row
// out and not enough to explain it; a shell that wanted the reason would
// have to re-derive the capability rule this machine owns, which is
// exactly what priority #2 forbids. So the reason ships beside the
// verdict, from the same shortfall computation.
//
// Note the deliberate asymmetry with `dns_provider_eligible` for an id
// outside `PROVIDERS`: that id is *ineligible* but has no row on screen,
// and there is no control to explain, so there is no reason to paint.
func (_self *OnboardingMachine) DnsProviderIneligibleReason(providerId string) *fauna_core.LocalizedText {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalLocalizedTextINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_dns_provider_ineligible_reason(
				_pointer, FfiConverterStringINSTANCE.Lower(providerId), _uniffiStatus),
		}
	}))
}

// Returns the DNS records the deferred-DNS orchestrator captured.
// Empty until `start_provisioning` runs the deferred path successfully.
// Read by clients on the `dns_post_instructions` page.
func (_self *OnboardingMachine) DnsRecords() []DnsRecordPlain {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterSequenceDnsRecordPlainINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_dns_records(
				_pointer, _uniffiStatus),
		}
	}))
}

func (_self *OnboardingMachine) DnsSetUpLater() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_dns_set_up_later(
			_pointer, _uniffiStatus)
		return false
	})
}

// i18n-aware variant of the DNS status text. Returns the i18n key plus
// the substitution map. Clients pass `(key, args)` through their
// platform's localization pipeline (Apple `Bundle.main.localizedString`,
// Android `getString`, Web `L()`, etc.). The string keys live in
// `i18n/strings/en.yaml` under `onboarding.dns_config.status_*`.
func (_self *OnboardingMachine) DnsStatusTextKey() fauna_core.LocalizedText {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return fauna_core.FfiConverterLocalizedTextINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_dns_status_text_key(
				_pointer, _uniffiStatus),
		}
	}))
}

// Derived view over `handle_check_snapshot().outcome` — NOT stored
// state. `DomainAvailable` ⟺ `Unregistered`, `RegisteredNoNest` ⟺
// itself; every other outcome (incl. `None` after a reset) has no
// domain-registration verdict to report. Sound because the outcome and
// this view share exactly one reset point (`reset_handle_check`, fired
// on identity change) and the outcome is otherwise stable for the
// lifetime of the DnsConfig step (`submit_handle_check_continue` reads
// it once to decide the transition and never mutates it further).
func (_self *OnboardingMachine) DomainStatus() *fauna_provisioning.DomainStatus {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalDomainStatusINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_domain_status(
				_pointer, _uniffiStatus),
		}
	}))
}

// Test-aware nest base URL: the override map wins
// when set; otherwise the wizard's state.nest_url is used.
//
// Use only for HTTP request URLs. For outcome values that identify
// the nest to per-app glue (e.g. `WizardOutcome::AwaitingManualDns`),
// read `state.nest_url` directly — the override must not leak into
// persisted outcome data.
func (_self *OnboardingMachine) EffectiveNestUrl() string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_effective_nest_url(
				_pointer, _uniffiStatus),
		}
	}))
}

// The identity the wizard is acting as — the secret every authenticating
// call signs with, and the one the app's wizard terminal persists
// (`onboarding.md` § Long-term store contract). `None` until the user has
// committed an identity. The rule (origin decides, other slot as fallback)
// and why it is a rule rather than a fixed precedence: [`State::canonical_secret`].
func (_self *OnboardingMachine) EffectiveSecret() *string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_effective_secret(
				_pointer, _uniffiStatus),
		}
	}))
}

// Whether the deployment's **mail** subsystem should be enabled at claim.
//
// **Machine-derived — there is no checkbox** (`onboarding.md` § 3b: the
// four claim-time enablement intents survive the retired
// `encryption_mode_choice` page as derived defaults, *not relocated* onto
// § 3b-bis, which stays confirm-only by design). The admin's change surface
// after onboarding is the admin-mail page.
//
// The value is the conjunction of two axes (ratified 2026-07-13): the
// handle-locality predicate ([`Self::handle_targets_real_domain`] — OFF
// for `user@localhost` / `user@IP`, which cannot hold MX/DKIM/TLS) AND
// the NAT axis ([`Self::effective_node_mode`]` != Private`). The NAT
// conjunct is how the two-box home-relay deployment
// (`deployment-home-with-public-relay.md`) says "this box runs no mail"
// with no checkbox: both boxes share one real-domain handle, but the home
// box is the one on the private axis — a claim-time enable there would
// mint a fresh MSEK, diverging from the fleet MSEK `LinkBoth` re-seals
// onto it. Still the onboarding→launch **hand-off channel**: the
// per-app launch glue reads this once `wizard_outcome() == LoggedIn` and,
// with its now-authenticated Admin client, calls
// `MailAdminClient::set_mail_enabled(true)` — keeping the Admin-class gate
// intact rather than opening a pre-identity admin surface. Idempotent with
// the mail-settings enable path; only meaningful on the admin-claim path.
// Per `docs/goal/behavior/mail-bridge-lifecycle.md` § Default-off on first
// claim.
func (_self *OnboardingMachine) EmailEnableRequested() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_email_enable_requested(
			_pointer, _uniffiStatus)
	}))
}

func (_self *OnboardingMachine) ErrorMessage() *string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_error_message(
				_pointer, _uniffiStatus),
		}
	}))
}

// Log a refused restore and hand the outcome back unchanged.
//
// The step is left alone on purpose: every refusal here is something the
// user acts on *from this screen* (fix the account, paste a different
// phrase, retry) — except `Superseded`, whose routing the client owns.
//
// It deliberately does **not** write `error_message`. The outcome is the
// contract, and the message that renders is the client's localized
// resolution of it ([`Self::set_error_message`]) — writing an English
// placeholder here would put a second, untranslated owner on the one
// channel `error-message` reads.
func (_self *OnboardingMachine) FailRecoveryEntry(outcome RecoveryEntryOutcome) RecoveryEntryOutcome {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterRecoveryEntryOutcomeINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_fail_recovery_entry(
				_pointer, FfiConverterRecoveryEntryOutcomeINSTANCE.Lower(outcome), _uniffiStatus),
		}
	}))
}

func (_self *OnboardingMachine) GeneratedSecret() *string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_generated_secret(
				_pointer, _uniffiStatus),
		}
	}))
}

// `trust-box-grant-button`: the user trusts this box with the default
// grant set. Latches the answer for the signed-in handoff — the wizard
// holds no authenticated session, so it cannot mint here — and concludes
// the wizard exactly as the NAT step would have.
func (_self *OnboardingMachine) GrantDefaultTrust() OnboardingStep {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOnboardingStepINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_grant_default_trust(
				_pointer, _uniffiStatus),
		}
	}))
}

func (_self *OnboardingMachine) HandleCheckSnapshot() HandleCheckSnapshot {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterHandleCheckSnapshotINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_handle_check_snapshot(
				_pointer, _uniffiStatus),
		}
	}))
}

// Step 1 of a `hosted-auth` field's sign-in: POST the device-authorization
// request to the form's `base-url` and hand back what the app must open
// (through its existing open-URL affordance) and show (the user code).
// The field flips to `Pending`; follow with [`Self::hosted_auth_wait`].
func (_self *OnboardingMachine) HostedAuthBegin(form CredentialForm, fieldId string) (HostedAuthPrompt, error) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*OnboardingError](
		FfiConverterOnboardingErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_onboarding_machine_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) HostedAuthPrompt {
			return FfiConverterHostedAuthPromptINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_hosted_auth_begin(
			_pointer, FfiConverterCredentialFormINSTANCE.Lower(form), FfiConverterStringINSTANCE.Lower(fieldId)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_rust_buffer(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}

// The `hosted-auth` button's label for one field, derived purely from
// [`Self::hosted_auth_state`] — the shared-Rust twin of the match arm
// tui's `hosted_auth_button` and linux's `paint_hosted_auth_button` each
// hand-copied and self-documented as a "mirror" of the other
// (`onboarding.md` § 4). android/apple/web keep their own copy: each
// binds a different platform i18n surface (`R.string.*`, a Swift
// `LocalizedStringKey`, a reactive TS `t.*`), so the enum→label mapping
// still has to be re-expressed per platform — only the two Rust-native
// apps could actually call one function.
func (_self *OnboardingMachine) HostedAuthButtonText(form CredentialForm, fieldId string) string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_hosted_auth_button_text(
				_pointer, FfiConverterCredentialFormINSTANCE.Lower(form), FfiConverterStringINSTANCE.Lower(fieldId), _uniffiStatus),
		}
	}))
}

// Whether the sign-in button is pressable: the form's `base-url` is
// filled in and no attempt on this field is mid-flight. Owned here so no
// app re-derives "which sibling field is the address" (priority #2).
func (_self *OnboardingMachine) HostedAuthCanBegin(form CredentialForm, fieldId string) bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_hosted_auth_can_begin(
			_pointer, FfiConverterCredentialFormINSTANCE.Lower(form), FfiConverterStringINSTANCE.Lower(fieldId), _uniffiStatus)
	}))
}

// Where a `hosted-auth` field's sign-in stands — the app's button label
// source (`onboarding.md` § 4). `Idle` for a field never started.
func (_self *OnboardingMachine) HostedAuthState(form CredentialForm, fieldId string) HostedAuthState {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterHostedAuthStateINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_hosted_auth_state(
				_pointer, FfiConverterCredentialFormINSTANCE.Lower(form), FfiConverterStringINSTANCE.Lower(fieldId), _uniffiStatus),
		}
	}))
}

// Step 2: poll the token endpoint at the server's interval until the user
// approves (the token lands in the form's credential bag under
// `field_id`, the field flips to `Connected`, and `can_verify_*` turns
// true) or the attempt ends (`Failed`). Resolves only then — the app
// awaits it the way it awaits `verify_dns`.
func (_self *OnboardingMachine) HostedAuthWait(form CredentialForm, fieldId string) error {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	_, err := uniffiRustCallAsync[*OnboardingError](
		FfiConverterOnboardingErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_hosted_auth_wait(
			_pointer, FfiConverterCredentialFormINSTANCE.Lower(form), FfiConverterStringINSTANCE.Lower(fieldId)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

	if err == nil {
		return nil
	}

	return err
}

func (_self *OnboardingMachine) IdentityOrigin() *IdentityOrigin {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalIdentityOriginINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_identity_origin(
				_pointer, _uniffiStatus),
		}
	}))
}

func (_self *OnboardingMachine) InviteError(ctx ErrorContext, transient bool, cause string) OnboardingStep {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOnboardingStepINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_invite_error(
				_pointer, FfiConverterErrorContextINSTANCE.Lower(ctx), FfiConverterBoolINSTANCE.Lower(transient), FfiConverterStringINSTANCE.Lower(cause), _uniffiStatus),
		}
	}))
}

func (_self *OnboardingMachine) InviteRequestSnapshot() InviteRequestSnapshot {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterInviteRequestSnapshotINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_invite_request_snapshot(
				_pointer, _uniffiStatus),
		}
	}))
}

func (_self *OnboardingMachine) IsBuyableViaProvider(domain string) bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_is_buyable_via_provider(
			_pointer, FfiConverterStringINSTANCE.Lower(domain), _uniffiStatus)
	}))
}

func (_self *OnboardingMachine) IsLoading() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_is_loading(
			_pointer, _uniffiStatus)
	}))
}

func (_self *OnboardingMachine) LocalNestReachable() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_local_nest_reachable(
			_pointer, _uniffiStatus)
	}))
}

// Snapshot for the `nat_mode_choice` page. Pure read.
// Per `docs/goal/behavior/onboarding.md` § 3b-bis.
func (_self *OnboardingMachine) NatModeSnapshot() NatModeSnapshot {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterNatModeSnapshotINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_nat_mode_snapshot(
				_pointer, _uniffiStatus),
		}
	}))
}

// Lands the wizard on `ClaimCode` with `nest_url` + `handle`
// pre-set and the snapshot reset to `Idle`. Per target
// `docs/goal/behavior/onboarding.md` § App-launch routing — silent-challenge
// fallback table (unclaimed-nest row): when /verify returns 404 AND
// `setup-status.claimed == false`, the saved nest is up but unclaimed,
// so the user must claim it themselves rather than ask for an invite.
// Pure state mutation; no IO.
func (_self *OnboardingMachine) NavigateToClaimCodeForKnownNest(nestUrl string, handle string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_navigate_to_claim_code_for_known_nest(
			_pointer, FfiConverterStringINSTANCE.Lower(nestUrl), FfiConverterStringINSTANCE.Lower(handle), _uniffiStatus)
		return false
	})
}

// Like [`navigate_to_claim_code_for_known_nest`], but also pre-loads a
// claim `code` the client already holds so the `ClaimCode` page can
// pre-fill its input (read via [`claim_code_prefill`]). Used by the
// factory-reset re-onboard affordance: `fauna.admin.factory_reset` returns
// the new claim code to the client and the human never sees it, so without
// pre-fill the admin would land on the claim-code page with nothing to
// type. After re-claim the nest is mode-unresolved, so the wizard still
// runs claim-code → encryption-mode (per
// `docs/goal/architecture/nest/storage-modes.md` § The claim-time choice).
// Pure state mutation; no IO.
func (_self *OnboardingMachine) NavigateToClaimCodeForKnownNestWithCode(nestUrl string, handle string, code string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_navigate_to_claim_code_for_known_nest_with_code(
			_pointer, FfiConverterStringINSTANCE.Lower(nestUrl), FfiConverterStringINSTANCE.Lower(handle), FfiConverterStringINSTANCE.Lower(code), _uniffiStatus)
		return false
	})
}

// Pre-seed a pending invite at app launch when the per-app long-term
// store has a record of one. Places the wizard at `InviteRequest` with
// `nest_url`/`handle`/the supplied snapshot loaded. Per-app glue calls
// this analogously to `seed_identity` after reading its store; the glue
// is responsible for the navigation that puts the user on the
// InviteRequest page.
//
// `status_json` is the JSON-serialized `InviteRequestState` enum from
// the per-app store. The wizard parses it back into a typed snapshot.
// If parsing fails (corrupt state, schema drift), the wizard falls back
// to `InviteRequestState::PendingReview` with the supplied request_id —
// the user can recheck and either resume or reset.
// Lands the wizard on `InviteRequest` with `nest_url` + `handle`
// pre-set and the snapshot reset to `Idle`. Per target
// `docs/goal/behavior/onboarding.md` §"App-launch routing": when the
// silent-challenge handshake reports the secret is unregistered on
// an otherwise-running nest, the user needs an invite — drop them
// directly on the invite-request page rather than make them
// re-type their handle. Pure state mutation; no IO.
//
// Distinct from `seed_pending_invite` (which restores a previously
// submitted request from the long-term store). Use this when there
// is no prior request to resume.
func (_self *OnboardingMachine) NavigateToInviteRequestForKnownNest(nestUrl string, handle string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_navigate_to_invite_request_for_known_nest(
			_pointer, FfiConverterStringINSTANCE.Lower(nestUrl), FfiConverterStringINSTANCE.Lower(handle), _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) NestUrl() string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_nest_url(
				_pointer, _uniffiStatus),
		}
	}))
}

// The pending-invite resume slot for the current state, or `None` when
// there is nothing to resume.
//
// **This replaced `submit_invite_request_continue()` (retired 2026-08-12).**
// That method existed to *exit* the wizard with
// `WizardOutcome::InviteSubmitted`, and the exit is what `onboarding.md`
// § Wizard exit handling deletes: the pending-review journey never leaves
// the `invite_request` page — it stays there and polls until the poll
// resolves to `LoggedIn`. Apps call this at the
// [`Self::wizard_submit_invite_request`] return instead, which § 3
// Persistence callouts names as "the only write moment".
//
// Returning the assembled slot (rather than three getters) is deliberate:
// the two rules that are silent when wrong — `state.nest_url` over
// `effective_nest_url()`, and opaque `status_json` — live once here
// instead of being re-derived by seven apps. See [`PendingInviteSlot`].
func (_self *OnboardingMachine) PendingInviteSlot() *PendingInviteSlot {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalPendingInviteSlotINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_pending_invite_slot(
				_pointer, _uniffiStatus),
		}
	}))
}

// Serializes the current `InviteRequestState` for the per-app
// pending-invite slot. The format is opaque to clients — the wizard
// parses it back via `seed_pending_invite`. Returns `""` on the
// unreachable case where serde fails (the enum derives Serialize).
func (_self *OnboardingMachine) PendingInviteStatusJson() string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_pending_invite_status_json(
				_pointer, _uniffiStatus),
		}
	}))
}

// Pre-identity probe used by **app-launch routing** to discriminate
// "secret unregistered + claimed nest → invite_request" from
// "secret unregistered + unclaimed nest → claim_code" (per
// `docs/goal/behavior/onboarding.md` § App-launch routing).
//
// Delegates to the active `NestApi`, so this rides the anonymous WS-RPC
// connection just like the rest of the wizard — replacing the legacy
// `GET /api/v1/setup-status` HTTP probe the web / launch-machine glue
// used to call directly. The orchestrator
// applies the safer-default fallback (`Err` → assume `claimed=true`);
// surfacing the raw error here keeps that decision in one place per
// caller rather than baking it into the machine.
func (_self *OnboardingMachine) ProbeSetupStatusAt(nestUrl string) (SetupStatus, error) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*ProbeError](
		FfiConverterProbeErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_onboarding_machine_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) SetupStatus {
			return FfiConverterSetupStatusINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_probe_setup_status_at(
			_pointer, FfiConverterStringINSTANCE.Lower(nestUrl)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_rust_buffer(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}

// Per-provider DNS-config status. Pure computation over the current
// snapshot. UIs consume this via WASM/UniFFI and switch UI shape on
// the variant; `can_continue_dns` consults it to decide whether
// Continue is enabled.
func (_self *OnboardingMachine) ProviderStatus() ProviderStatus {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterProviderStatusINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_provider_status(
				_pointer, _uniffiStatus),
		}
	}))
}

// Whether the `vps-config-mail-mode-toggle` is ON. Returns the user's
// explicit choice if set, else the handle's real-domain default
// ([`Self::handle_targets_real_domain`]) — the same predicate that seeds the
// §3b enable-email default, so a real-domain box defaults to a mail box and a
// `localhost` / IP target defaults to social-only. Clients read this to
// render the toggle's checked state and to gate the server-type radio (see
// [`server_type_allowed_for_mail`]). Per `docs/goal/behavior/onboarding.md`
// §5.
func (_self *OnboardingMachine) ProvisionMailModeEnabled() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_provision_mail_mode_enabled(
			_pointer, _uniffiStatus)
	}))
}

// The box's reach address once `create_server` has returned, else `None`
// (§ 6 *Reaching the box*). Survives a `reset()` no more and no less than
// the rest of the run state does — see [`Self::reset`].
func (_self *OnboardingMachine) ProvisionReachIpv4() *string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_provision_reach_ipv4(
				_pointer, _uniffiStatus),
		}
	}))
}

// The selected update channel — the user's explicit choice if set, else
// the default (`stable`). Clients read this to mark the selected
// `vps-config-update-channel-row`.
func (_self *OnboardingMachine) ProvisionUpdateChannel() fauna_provisioning.UpdateChannel {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return fauna_provisioning.FfiConverterUpdateChannelINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_provision_update_channel(
				_pointer, _uniffiStatus),
		}
	}))
}

// Why the wizard-exit Continue button is dead — `ui/README.md` rule 5:
// the four `○` step glyphs are a symbol, not a reason. One message per
// blocked `OverallStatus` (idle/running/failed/cancelled), because the
// user's next act differs: start, wait, retry.
func (_self *OnboardingMachine) ProvisioningContinueBlockedReason() *fauna_core.LocalizedText {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalLocalizedTextINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_provisioning_continue_blocked_reason(
				_pointer, _uniffiStatus),
		}
	}))
}

// Cancel button is shown — provisioning is actively running. Also gates
// the elapsed-time ticker.
func (_self *OnboardingMachine) ProvisioningInProgress() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_provisioning_in_progress(
			_pointer, _uniffiStatus)
	}))
}

// Returns a clone of the current provisioning snapshot. Cheap (clones
// a small struct). Pull-based — clients re-read on every observer
// tick rather than receiving the snapshot via callback.
//
// The clone is `enrich_display`-ed so every app reads the canonical
// per-step visibility booleans (`shows_substep`/`shows_error`/
// `shows_attempt_suffix`) instead of re-deriving the rule. Computed here on
// the outgoing clone — never on the live mutated state, which the
// `set_cancelled` path mutates outside `with_step` (see `recompute_display`).
func (_self *OnboardingMachine) ProvisioningSnapshot() fauna_provisioning.ProvisioningSnapshot {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return fauna_provisioning.FfiConverterProvisioningSnapshotINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_provisioning_snapshot(
				_pointer, _uniffiStatus),
		}
	}))
}

func (_self *OnboardingMachine) RecheckInviteStatus() OnboardingStep {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	res, _ := uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_onboarding_machine_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) OnboardingStep {
			return FfiConverterOnboardingStepINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_recheck_invite_status(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_rust_buffer(handle)
		},
	)

	return res
}

// Polls the freshly-provisioned nest from the post-provisioning
// "Almost ready" surface. The client calls this on a timer while
// `wizard_outcome()` is `AwaitingManualDns` (the deferred-DNS exit),
// single-shot — exactly like `recheck_invite_status`: one
// `probe_setup_status` reachability+claim-status probe over WS-RPC, and
// if the nest is reachable and unclaimed, one `claim_admin` call.
//
// Every call here is **pre-claim**: it rides the anonymous WS-RPC
// connection — there is no authenticated actor session until the claim
// succeeds. (A `setup-status` failure means DNS hasn't propagated yet or
// the nest is still booting; both surface to the user as "waiting for
// your nest to come online".) On a successful claim the wizard routes to
// `NatModeChoice` — the same path `wizard_submit_claim_code` takes —
// clearing the awaiting outcome. On the already-claimed resume it skips
// that setup step and concludes through § 3b-ter's offer instead
// (`TrustPrompt` on an app that renders it, else straight to `Done`).
//
// No-op (returns the current step) unless `wizard_outcome()` is
// `AwaitingManualDns`. Transient failures (nest unreachable, 5xx on
// claim) leave the snapshot `Pending` so the client keeps polling; a
// rejected claim, a corrupt identity, or a first-contact identity
// mismatch (security.md § Pre-claim surfacing) yields a terminal
// `Error`. Per `docs/goal/behavior/onboarding.md` § "Wizard exit
// handling".
func (_self *OnboardingMachine) RecheckManualDns() OnboardingStep {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	res, _ := uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_onboarding_machine_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) OnboardingStep {
			return FfiConverterOnboardingStepINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_recheck_manual_dns(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_rust_buffer(handle)
		},
	)

	return res
}

// `recover-method-cloud-button` on `nest_recovery`: re-provision the
// selected box via a cloud VPS. Advances to `vps_config` in recovery mode
// (the shared orchestrator re-provisions with the saved seed installed and
// re-points A/AAAA as its `Dns` step — the drive is a later slice). Errors
// if no box is selected.
func (_self *OnboardingMachine) RecoverViaCloud() error {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	_, _uniffiErr := rustCallWithError[*OnboardingError](FfiConverterOnboardingError{}, func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_recover_via_cloud(
			_pointer, _uniffiStatus)
		return false
	})
	return _uniffiErr.AsError()
}

// `recover-method-selfhosted-button` on `nest_recovery`: re-provision the
// selected box via a self-hosted installer. Advances to
// `recover_selfhosted_instructions` (the installer command carrying
// `FAUNA_DEPLOYMENT_SEED`). Errors if no box is selected.
func (_self *OnboardingMachine) RecoverViaSelfhosted() error {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	_, _uniffiErr := rustCallWithError[*OnboardingError](FfiConverterOnboardingError{}, func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_recover_via_selfhosted(
			_pointer, _uniffiStatus)
		return false
	})
	return _uniffiErr.AsError()
}

// The custodied box list rendered on `nest_recovery` — one `nest_actor_id`
// (hex) per box, empty when nothing is custodied / not yet synced
// (`recover-box-empty-message`).
func (_self *OnboardingMachine) RecoveryBoxes() []string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterSequenceStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_recovery_boxes(
				_pointer, _uniffiStatus),
		}
	}))
}

// Which entry the recovery branch was reached from, or `None` outside it.
// The glue uses this to route `recover-back-button` (came-from-launch tears
// the wizard down to `launch_retry`; came-from-identity is `back()`).
func (_self *OnboardingMachine) RecoveryCameFrom() *BoxRecoveryEntry {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalBoxRecoveryEntryINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_recovery_came_from(
				_pointer, _uniffiStatus),
		}
	}))
}

// Whether the wizard is in the total-box-loss recovery branch. The web glue
// gates the entry CTAs / recovery-mode provisioning on this.
func (_self *OnboardingMachine) RecoveryIntent() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_recovery_intent(
			_pointer, _uniffiStatus)
	}))
}

// The minted-but-unregistered RecoveryKey root the `recovery_kit` screen
// displays (64-hex + QR). `None` outside the kit screen's lifetime.
// Hands out a plain `String` at the render boundary (the foreign string
// is un-zeroizable by design — `fauna_core::secret` module docs); the
// held copy stays the zeroizing, `Debug`-redacted [`SecretString`].
func (_self *OnboardingMachine) RecoveryKitSecretHex() *string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_recovery_kit_secret_hex(
				_pointer, _uniffiStatus),
		}
	}))
}

// The `fauna://recovery` URI behind the `recovery_kit` screen's QR **and**
// its copy button — one payload for both (`identity-succession.md` § The
// RecoveryKey, *Which encoding each affordance carries*); the display
// alone is the bare [`Self::recovery_kit_secret_hex`]. It names the
// account (the actor the generated identity derives to) and truthfully no
// handle — none is chosen yet at this position, so a restore from it asks
// (`recovery-entry-account-field`). `None` outside the screen's lifetime.
// Built here so every app renders the same payload (priority #2).
func (_self *OnboardingMachine) RecoveryKitUri() *string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_recovery_kit_uri(
				_pointer, _uniffiStatus),
		}
	}))
}

// The `nest_actor_id` (hex) of the selected box on `nest_recovery`, or
// `None` before a selection. Drives the `recover-box-item` selected state.
func (_self *OnboardingMachine) RecoverySelectedNestId() *string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_recovery_selected_nest_id(
				_pointer, _uniffiStatus),
		}
	}))
}

func (_self *OnboardingMachine) RedeemInvite() OnboardingStep {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	res, _ := uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_onboarding_machine_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) OnboardingStep {
			return FfiConverterOnboardingStepINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_redeem_invite(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_rust_buffer(handle)
		},
	)

	return res
}

// Mint the single-use nonce the platform attestation binds
// (`fauna.account.age_nonce`, pre-identity, against the wizard's current
// nest). The app then runs its attestation round over
// [`Self::age_claim_message`] inside `expires_in_secs` and hands the
// result to [`Self::set_age_claim`]. Minted from the nest the admission
// will go to (`effective_nest_url`, the URL both admission paths use), and
// remembered — with the platforms the reply listed — for the send guard.
func (_self *OnboardingMachine) RequestAgeNonce() (AgeNoncePlain, error) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*OnboardingError](
		FfiConverterOnboardingErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_onboarding_machine_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) AgeNoncePlain {
			return FfiConverterAgeNoncePlainINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_request_age_nonce(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_rust_buffer(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}

// Discards all state and resets to `IdentityChoice`.
//
// Also clears the provisioning snapshot + cancel flag so a "start over"
// (or the E2E `reset` between tests) returns to a clean Idle provisioning
// page. Native apps reuse one long-lived machine across resets, so
// without this a prior run's terminal snapshot (e.g. `Succeeded`) would
// persist and hide the `provisioning-start-button` (visible only while
// `overall == Idle`) — web avoids it only because its reset reconstructs
// the machine. The claim-code snapshot is cleared for the identical reason:
// its terminal `Claimed` state carries `submit_enabled == false`, so a
// process that already claimed a nest would render the next claim's Submit
// button permanently dead. (Its route in re-seeds too — see the
// `UnregisteredUnclaimedNest` arm — but "start over" must not depend on
// which door the user next walks through.) The remaining transient
// snapshots (handle-check, invite) are re-seeded by their own setters at
// the start of every use, so they don't need clearing here.
func (_self *OnboardingMachine) Reset() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_reset(
			_pointer, _uniffiStatus)
		return false
	})
}

// Disambiguate a `NotFound` recheck by asking whether this identity is now
// *registered* on the nest — the "approval is detected as admission" rule
// (`onboarding.md` § The pending-invite surface).
//
// The request row is gone either because the admin approved it (the approve
// creates the account, then deletes the row) or because it was cancelled /
// purged. Nothing in the row's absence separates those, and an anonymous
// poller has no notification channel to be told which happened (every
// notification plane is keyed on a bearer-proven `actor_id` this caller does
// not have until approval creates it). So we run the same
// `fauna.auth.{challenge,verify}` ceremony the silent sign-in uses, with the
// identity we already hold:
//
// - **registered** → that verify *is* the login. Route to `LoggedIn`/`Done`
// exactly like a redeem success — no "Approved, press Continue"
// interstitial, mirroring the awaiting-DNS surface's auto-proceed.
// - **not registered** → the request is genuinely gone: terminal
// `invite.error.not_found`, on which the per-app glue deletes the slot.
// - **couldn't tell** (unreachable, degraded nest, unusable secret) → stay
// `PendingReview` and let the next poll tick ask again. Deliberately NOT
// an `Error` state: `recheck_invite_status` only proceeds from
// `PendingReview`, so writing any error here would wedge the poll
// permanently — a dropped connection would strand a live request behind a
// terminal, slot-deleting screen.
func (_self *OnboardingMachine) ResolveNotFoundRecheck(nestUrl string, secret string, pendingRequestId string) OnboardingStep {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	res, _ := uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_onboarding_machine_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) OnboardingStep {
			return FfiConverterOnboardingStepINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_resolve_not_found_recheck(
			_pointer, FfiConverterStringINSTANCE.Lower(nestUrl), FfiConverterStringINSTANCE.Lower(secret), FfiConverterStringINSTANCE.Lower(pendingRequestId)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_rust_buffer(handle)
		},
	)

	return res
}

// The nest URL a restore connects to, resolved from the account handle's
// domain exactly as the handle-check resolves its probe target: the local
// / loopback classification first, then SRV port discovery for a public
// name that carried no explicit port, so a clean `alice@domain` stays
// port-hidden.
//
// The `"nest"` provider override wins when set, the same way
// `run_handle_check_phases` lets it win over `target.base_url`. It has to
// be resolved **here**, not left to `WsNestApi`: that type captures the
// map once at construction, while the E2E bridge installs the override at
// runtime through `set_provider_base_urls`, so a machine-side caller that
// skips this step dials the resolved `https://` URL at a harness nest
// serving plain HTTP (which surfaces as a corrupt-message WS handshake —
// the failure this method's own tier_3 journey caught).
func (_self *OnboardingMachine) ResolveRestoreTarget(domain string) string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	res, _ := uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_onboarding_machine_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) string {
			return FfiConverterStringINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_resolve_restore_target(
			_pointer, FfiConverterStringINSTANCE.Lower(domain)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_rust_buffer(handle)
		},
	)

	return res
}

// Predecessor identity seeds a phrase-only restore recovered alongside the
// account's own (`identity-succession.md` § Seed escrow). **Empty in every
// ordinary onboarding**; non-empty only when the restored account is mid
// corpus re-seal after an identity succession.
//
// The client persists these into its account registry beside the restored
// identity — they are what lets the re-seal driver open a corpus still
// sealed under a predecessor. Read at the same point as
// [`Self::effective_secret`]: this machine holds no store, so an
// unread value is simply lost when the wizard ends.
func (_self *OnboardingMachine) RestoredPredecessors() []RestoredPredecessorSeed {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterSequenceRestoredPredecessorSeedINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_restored_predecessors(
				_pointer, _uniffiStatus),
		}
	}))
}

// Why the restore recovered no predecessor material even though the blob
// carried a section — see
// [`RecoveryEntryOutcome::RestoredPredecessorsLost`].
func (_self *OnboardingMachine) RestoredPredecessorsUnreadable() *string {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_restored_predecessors_unreadable(
				_pointer, _uniffiStatus),
		}
	}))
}

// Re-runs provisioning from the top. Idempotency makes already-done
// steps short-circuit on re-run, so this is functionally equivalent
// to "resume from the failed step." Same fire-and-forget shape as
// `start_provisioning`.
//
// Resuming means the SAME box: the re-run reuses the claim code and
// expects the identity the box was built with (`pending_provision_row`),
// rather than minting fresh ones a box that already exists can never
// match. Only the run's own progress snapshot is reset here.
func (_self *OnboardingMachine) RetryProvisioning() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_retry_provisioning(
			_pointer, _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) RunHandleCheckPhases(handle string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_run_handle_check_phases(
			_pointer, FfiConverterStringINSTANCE.Lower(handle)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

}

// Runs the four-step orchestrator **to completion** (the future
// `start_provisioning` spawns) and stashes the terminal result. Observer
// ticks drive re-render throughout; `provisioning_snapshot()` / the
// `provisioning_cancel` flag stay readable/settable from another thread.
//
// `start_provisioning` is the fire-and-forget spawn wrapper used by web
// (`spawn_local`) and any native caller already inside a tokio runtime.
// Native apps that have **no** runtime on their UI thread during
// onboarding (the GTK main thread on linux — `tokio::spawn` there would
// panic) instead `await` this on a worker runtime
// (`async_helper::run_on_tokio`). Idempotent like `start_provisioning`:
// `run_provisioning_inner` resets the snapshot + cancel flag at entry, so
// it doubles as the retry/resume entry point.
func (_self *OnboardingMachine) RunProvisioning() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_run_provisioning(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

}

// Place the wizard at `InviteRequest` with handle and nest_url
// hydrated, but no pending-invite record. Used by the per-app
// app-launch glue when the silent challenge reports the secret
// isn't registered on a known-good nest — the user is still on
// the right nest, just needs an invite. Equivalent in shape to
// `seed_pending_invite` but without a `request_id` or status JSON.
// Per the onboarding client target-state design's app-launch
// failure-mode table (tracked internally).
func (_self *OnboardingMachine) SeedAtInviteRequestUnregistered(handle string, nestUrl string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_seed_at_invite_request_unregistered(
			_pointer, FfiConverterStringINSTANCE.Lower(handle), FfiConverterStringINSTANCE.Lower(nestUrl), _uniffiStatus)
		return false
	})
}

// App-launch hydration for the "Almost ready" surface. Lands the wizard
// at the deferred-DNS exit from a previously-saved slot. The per-app
// glue calls `seed_identity(secret)` first (so signing works for the
// claim), then this with the persisted (nest_url, handle, dns_records,
// claim_code). Sets `wizard_outcome()` to `AwaitingManualDns` — exactly
// what the same-session `continue_from_dns_post_instructions` exit
// produces — so the client renders the surface identically whether it
// arrived this session or on relaunch. The client then polls
// `recheck_manual_dns()`. Mirrors `seed_pending_invite` /
// `seed_pending_encryption_mode_choice`. Per
// `docs/goal/behavior/onboarding.md` § "Wizard exit handling".
//
// Clients holding the slot's opaque `dns_records_json` should prefer
// [`Self::seed_awaiting_manual_dns_json`], which takes it verbatim.
func (_self *OnboardingMachine) SeedAwaitingManualDns(nestUrl string, handle string, dnsRecords []DnsRecordPlain, claimCode string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_seed_awaiting_manual_dns(
			_pointer, FfiConverterStringINSTANCE.Lower(nestUrl), FfiConverterStringINSTANCE.Lower(handle), FfiConverterSequenceDnsRecordPlainINSTANCE.Lower(dnsRecords), FfiConverterStringINSTANCE.Lower(claimCode), _uniffiStatus)
		return false
	})
}

// Relaunch hydration straight from the awaiting-DNS slot, taking the slot's
// opaque `dns_records_json` verbatim.
//
// The companion to [`Self::awaiting_dns_records_json`]: together they keep
// the record list **opaque to the client on both ends**, so a non-Rust
// client never builds or parses that JSON. That is what stops the bindings'
// camelCase (`recordType`) from silently round-tripping through serde's
// snake_case (`record_type`) to an *empty* list — a failure that would only
// surface after a relaunch, as an "Almost ready" page listing no records.
//
// A corrupt slot degrades to an empty record list rather than a panic: the
// user still reaches the surface (and can re-run the DNS step) instead of
// crashing on launch.
func (_self *OnboardingMachine) SeedAwaitingManualDnsJson(nestUrl string, handle string, dnsRecordsJson string, claimCode string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_seed_awaiting_manual_dns_json(
			_pointer, FfiConverterStringINSTANCE.Lower(nestUrl), FfiConverterStringINSTANCE.Lower(handle), FfiConverterStringINSTANCE.Lower(dnsRecordsJson), FfiConverterStringINSTANCE.Lower(claimCode), _uniffiStatus)
		return false
	})
}

// Relaunch hydration straight from the awaiting-DNS slot, taking the
// **whole record** as the launch flow read it
// (`fauna_launch_machine::AwaitingDnsRecord`) — the door every app's
// relaunch glue should walk through, so a field the slot grows (the
// reach address, the box's built-with identity) reaches the machine with
// no per-app change at all. The two field-taking forms above and below
// delegate here.
//
// Beyond what they seed, this re-holds the identity the box was built
// with (`record.nest_actor_id`, `security.md` § Transport trust — the
// *Client-provisioned box* row's "no TOFU window" holds across a
// relaunch only because the slot carries the root) as the first-contact
// root for the identity URL's host, and retains the row as the box this
// machine is mid-way through provisioning, so a "start over" onto the
// same domain after the relaunch resumes it rather than minting a fresh
// identity of it (§ 6 *The pending-provision slot*).
func (_self *OnboardingMachine) SeedAwaitingManualDnsRecord(record fauna_launch_machine.AwaitingDnsRecord) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_seed_awaiting_manual_dns_record(
			_pointer,
			CFromRustBuffer(fauna_launch_machine.FfiConverterAwaitingDnsRecordINSTANCE.LowerExternal(record)), _uniffiStatus)
		return false
	})
}

// Pre-seed the wizard with an identity loaded from the client's
// long-term store at app launch. Skips identity-creation/import and
// places the wizard at HandleEntry. No validation — the caller is
// responsible for providing a valid 64-char hex secret it owns.
//
// `identity_origin` is left `None` (not `Imported`) so that pressing
// Back from `HandleEntry` routes to `IdentityChoice` — the seeded
// user never visited `IdentityImport`/`IdentityCreated` in this
// session, so routing them there would show an empty paste form they
// can't act on. From `IdentityChoice` they can re-pick if they want
// to overwrite the seeded identity.
func (_self *OnboardingMachine) SeedIdentity(secret string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_seed_identity(
			_pointer, FfiConverterStringINSTANCE.Lower(secret), _uniffiStatus)
		return false
	})
}

// `launch-recover-button` on `launch_retry` (the surviving-device entry).
// The client already holds its identity, so the glue passes it here; this
// seeds it and drops straight into `NestRecovery` (the synced
// `fauna.state.deployment-seeds` map is guaranteed local). Mirrors `seed_identity`, but for the
// recovery branch. No validation — the caller owns a valid 64-hex secret.
func (_self *OnboardingMachine) SeedIdentityForRecovery(secret string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_seed_identity_for_recovery(
			_pointer, FfiConverterStringINSTANCE.Lower(secret), _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) SeedPendingInvite(nestUrl string, handle string, requestId string, statusJson string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_seed_pending_invite(
			_pointer, FfiConverterStringINSTANCE.Lower(nestUrl), FfiConverterStringINSTANCE.Lower(handle), FfiConverterStringINSTANCE.Lower(requestId), FfiConverterStringINSTANCE.Lower(statusJson), _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) SelectDnsProvider(id string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_select_dns_provider(
			_pointer, FfiConverterStringINSTANCE.Lower(id), _uniffiStatus)
		return false
	})
}

// User picked Public or Private on the `nat_mode_choice` page
// (`public-nat-mode-radio` / `private-nat-mode-radio`). Sets
// `selected_mode` and returns the snapshot to `Choosing` — including from
// `Error` (re-picking recovers); submit stays enabled.
// Per `docs/goal/behavior/onboarding.md` § 3b-bis.
func (_self *OnboardingMachine) SelectNatMode(mode fauna_core.NodeMode) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_select_nat_mode(
			_pointer,
			CFromRustBuffer(fauna_core.FfiConverterNodeModeINSTANCE.LowerExternal(mode)), _uniffiStatus)
		return false
	})
}

// Select a custodied box on `nest_recovery` (`recover-box-item`). Records
// the box's `nest_actor_id` (hex); the method buttons stay disabled until a
// box is selected. The raw seed is resolved in Rust at re-provision time
// (`deployment_seed_for`), never surfaced here.
func (_self *OnboardingMachine) SelectRecoveryBox(nestActorId string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_select_recovery_box(
			_pointer, FfiConverterStringINSTANCE.Lower(nestActorId), _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) SelectVpsLocation(id string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_select_vps_location(
			_pointer, FfiConverterStringINSTANCE.Lower(id), _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) SelectVpsProvider(id string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_select_vps_provider(
			_pointer, FfiConverterStringINSTANCE.Lower(id), _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) SelectVpsServerType(id string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_select_vps_server_type(
			_pointer, FfiConverterStringINSTANCE.Lower(id), _uniffiStatus)
		return false
	})
}

// Whether the currently-selected DNS registrar requires per-
// registration WHOIS contact info (Gandi today; Porkbun = false).
// Drives the `dns-contact-form` visibility per the target-state
// doc's §4. Returns `false` when no provider is selected, when
// the provider has no `registrar` capability, or when no creds
// have been entered yet — the form only appears once a registrar
// has been chosen and (post-`verify_dns`) the wizard has
// confirmed the provider's contact requirements.
func (_self *OnboardingMachine) SelectedRegistrarRequiresContact() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_selected_registrar_requires_contact(
			_pointer, _uniffiStatus)
	}))
}

// Set (or clear) the age claim the next admission carries
// (`family-safety.md` § The account age band). The mobile apps call this
// with their store age signal — attested when the platform attestation
// round succeeded, declared-only otherwise; every other app never calls
// it. Both `submit_invite_request` and `register` carry it through
// [`Self::age_claim_to_send`], which strips an attestation the addressed
// nest did not say it can check.
func (_self *OnboardingMachine) SetAgeClaim(claim *AgeClaimPlain) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_age_claim(
			_pointer, FfiConverterOptionalAgeClaimPlainINSTANCE.Lower(claim), _uniffiStatus)
		return false
	})
}

// Sets the WHOIS contact for the buy-domain path. Called by the per-
// client UI's contact form on submit. Replaces any contact previously
// prefilled by `verify_dns` via `Registrar::fetch_default_contact()`.
func (_self *OnboardingMachine) SetContact(contact fauna_provisioning.ContactInfo) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_contact(
			_pointer,
			CFromRustBuffer(fauna_provisioning.FfiConverterContactInfoINSTANCE.LowerExternal(contact)), _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) SetControlCheckbox(checked bool) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_control_checkbox(
			_pointer, FfiConverterBoolINSTANCE.Lower(checked), _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) SetCurrentHandle(h string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_current_handle(
			_pointer, FfiConverterStringINSTANCE.Lower(h), _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) SetDnsCred(fieldId string, value string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_dns_cred(
			_pointer, FfiConverterStringINSTANCE.Lower(fieldId), FfiConverterStringINSTANCE.Lower(value), _uniffiStatus)
		return false
	})
}

// Put a client-resolved message on the shared `error-message` channel.
//
// The counterpart of [`Self::begin_import_identity_with_reason`]'s
// `reason`, without the transition: the machine holds no string table, so
// a surface whose outcome is a typed value (today
// [`RecoveryEntryOutcome`]) resolves the `onboarding.*` key its client-side
// string table owns and stores the result here, where every app's
// `error-message` already reads from. Keeping it on the machine rather
// than in per-app view state is what makes the message survive the
// observer tick that fires with it (the same reasoning
// `begin_import_identity_with_reason` documents).
func (_self *OnboardingMachine) SetErrorMessage(message string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_error_message(
			_pointer, FfiConverterStringINSTANCE.Lower(message), _uniffiStatus)
		return false
	})
}

// Apply a **nest hint** to the handle-entry page: pre-fill the handle's
// domain part with `raw` when it classifies as a nest target, and report
// whether it did (`onboarding.md` § 2 Handle entry → *Nest hint*).
//
// On web the hint is the `nest` query parameter a nest's central-origin
// redirect carries (`web-content-hosting.md` § The nest-served `/app/`
// and the central origin); a native deep link can hand the same raw value
// here later. Parsing and the drop rule live HERE so every app applies the
// same rule and none reads the raw hint into its own state:
//
// - The hint must name exactly one `host[:port]` authority
// (`fauna_core::web::is_domain_authority_syntax` — no userinfo, path,
// query or whitespace) before the shared probe classifier
// (`resolve_handle_domain`) is trusted with it: that classifier is a
// negative test and would call `evil.example/x?y` "public".
// - A hint that does not classify is **dropped silently** — the field is
// left as it was and `false` comes back; never an error message.
// - It is a prefill and nothing more: any local part already typed is
// kept, the step does not change, and the check, probe and trust
// ceremony run exactly as for a typed domain. App-launch routing runs
// before onboarding and never consults the hint, so a signed-in app
// never reaches this call.
func (_self *OnboardingMachine) SetNestHint(raw string) bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_nest_hint(
			_pointer, FfiConverterStringINSTANCE.Lower(raw), _uniffiStatus)
	}))
}

func (_self *OnboardingMachine) SetNestUrl(url string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_nest_url(
			_pointer, FfiConverterStringINSTANCE.Lower(url), _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) SetPhase(phase HandleCheckPhase, msgKey string, args map[string]string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_phase(
			_pointer, FfiConverterHandleCheckPhaseINSTANCE.Lower(phase), FfiConverterStringINSTANCE.Lower(msgKey), FfiConverterMapStringStringINSTANCE.Lower(args), _uniffiStatus)
		return false
	})
}

// Set the `vps-config-mail-mode-toggle`: whether the box provisions the mail
// subsystem. `true` = a mail box (clamd/rspamd scanner sidecars + mail ports
// `{25,465,587,993}`; needs a `mem_gb ≥ 2`
// plan); `false` = a social-only box (lean nest+watchtower compose, viable on
// the 1 GB tier — but it cannot later enable mail without a VPS resize). The
// decision must live here at `vps_config`, not the post-claim §3b
// enable-email checkbox, because cloud-init must know mail-intent (and the
// RAM, picked on this same page) *before* the box boots. Drives
// `CloudInitParams::enable_mail` via `run_provisioning_inner`. Defaults from
// [`Self::provision_mail_mode_enabled`] (the handle's real-domain default)
// until the user toggles it. Per `docs/goal/behavior/onboarding.md` §5.
func (_self *OnboardingMachine) SetProvisionMailMode(enabled bool) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_provision_mail_mode(
			_pointer, FfiConverterBoolINSTANCE.Lower(enabled), _uniffiStatus)
		return false
	})
}

// Select a `vps-config-update-channel-row`: which builds the box's
// automatic updater follows. Decided at `vps_config`, like the mail mode,
// because the channel's image tag is written into the box's cloud-init.
// Drives `CloudInitParams::image_tag` via `run_provisioning_inner`. Per
// `docs/goal/behavior/onboarding-provisioning.md` §5.
func (_self *OnboardingMachine) SetProvisionUpdateChannel(channel fauna_provisioning.UpdateChannel) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_provision_update_channel(
			_pointer,
			CFromRustBuffer(fauna_provisioning.FfiConverterUpdateChannelINSTANCE.LowerExternal(channel)), _uniffiStatus)
		return false
	})
}

// Push the custodied box list into the machine for `nest_recovery` to
// render (`recover-box-item`). The per-app glue fetches it from a
// reachable nest via the shared `deploymentSeeds()` getter (owner-secret
// read of `fauna.state.deployment-seeds`) and hands the resulting `nest_actor_id` (hex) list
// here — held on the machine so every app renders one uniform list
// (like `verify_vps` populating `vps.server_types`). Only public
// `nest_actor_id`s cross; the seed stays in custody. A selection that is
// no longer in the new list is cleared.
func (_self *OnboardingMachine) SetRecoveryBoxes(boxes []string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_recovery_boxes(
			_pointer, FfiConverterSequenceStringINSTANCE.Lower(boxes), _uniffiStatus)
		return false
	})
}

// App capability declaration: this app has the `recovery_kit` onboarding
// screen built (`onboarding.md` § 1 Identity). Call once at wizard
// construction; the machine then routes `confirm_generated_identity`
// through the kit screen. Apps that never call it keep the pre-existing
// straight-to-`HandleEntry` flow — the batched-trickle-down parity gap.
func (_self *OnboardingMachine) SetRendersRecoveryKit(renders bool) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_renders_recovery_kit(
			_pointer, FfiConverterBoolINSTANCE.Lower(renders), _uniffiStatus)
		return false
	})
}

// App capability declaration: this app has the `trust_prompt` onboarding
// screen built (`onboarding.md` § 3b-ter). Call once at wizard
// construction; the machine then routes every *arrival* exit through the
// one-tap trust offer — both NAT-page exits on the claim path, and both
// join routes (invite redemption, approved request). Apps that never call
// it keep the pre-existing straight-to-`Done` flow — the
// batched-trickle-down parity gap, exactly as
// [`Self::set_renders_recovery_kit`] works.
func (_self *OnboardingMachine) SetRendersTrustPrompt(renders bool) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_renders_trust_prompt(
			_pointer, FfiConverterBoolINSTANCE.Lower(renders), _uniffiStatus)
		return false
	})
}

// Re-root the re-provision drive's custody read at a sandboxed phone
// shell's account-store container — the same container the shell hands the
// account runtime (`fauna_client_account_runtime::SandboxedStoreContainer`),
// so the local read opens the store the runtime writes. `None` restores the
// per-OS platform root, which every desktop host and web use without ever
// calling this.
func (_self *OnboardingMachine) SetStoreContainerDir(storeContainerDir *string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_store_container_dir(
			_pointer, FfiConverterOptionalStringINSTANCE.Lower(storeContainerDir), _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) SetVpsCred(fieldId string, value string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_set_vps_cred(
			_pointer, FfiConverterStringINSTANCE.Lower(fieldId), FfiConverterStringINSTANCE.Lower(value), _uniffiStatus)
		return false
	})
}

// Whether the dns_config page should render the WHOIS contact form. True
// only when the selected registrar requires a contact AND the domain is
// unregistered-but-buyable through it (`provider_status()` ==
// `UnregisteredBuyable`, i.e. the buy-domain path actually runs). Clients
// gate the contact form on this instead of re-combining
// `selected_registrar_requires_contact()` with `provider_status()`.
func (_self *OnboardingMachine) ShouldShowContactForm() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_should_show_contact_form(
			_pointer, _uniffiStatus)
	}))
}

// Whether the dns_config page should show the "no supported registrar
// carries .{tld}" message. True when the user must buy the domain
// (`buy_domain`) AND the handle-check probe found the domain available
// but not buyable via any supported registrar
// (`HandleCheckOutcome::DomainAvailable { buyable_via_provider: false }`).
//
// This is a TLD property knowable from the handle check alone: it does
// NOT require the user to first select + verify a provider, and it does
// not depend on *which* provider is selected. (Re-deriving it from
// `provider_status() == UnregisteredNotBuyable` — as iOS/web/android did
// before this getter — is both late (that status is `NotReady` until a
// provider is selected + verified) and provider-specific (it would
// mislabel a TLD that some registrar carries but the *selected* one does
// not).) The message text is about the TLD, so the handle-check semantic
// is the canonical one (`onboarding.md` § 4). Clients gate
// `dns-no-provider-message` on this getter instead of re-deriving it
// (priority #2; mirrors `should_show_contact_form` /
// `should_show_registrar_notes`).
func (_self *OnboardingMachine) ShouldShowNoProviderMessage() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_should_show_no_provider_message(
			_pointer, _uniffiStatus)
	}))
}

// Whether the registrar-specific notes blurb (e.g. Porkbun's
// account-contact requirement) should be shown: only on the buy-domain
// path, and only when the selected provider declares a
// `registrar_notes_key`.
func (_self *OnboardingMachine) ShouldShowRegistrarNotes() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_should_show_registrar_notes(
			_pointer, _uniffiStatus)
	}))
}

// `recovery-kit-skip-button`: decline the kit. One click, never blocks
// onboarding; the minted root is dropped (zeroized) so nothing registers
// at handoff, and Settings' never-created warning tells the truth.
func (_self *OnboardingMachine) SkipRecoveryKit() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_skip_recovery_kit(
			_pointer, _uniffiStatus)
		return false
	})
}

// `trust-box-skip-button`: decline. "Declining leaves everything as
// today" (§ 3b-ter) — nothing is latched, nothing is minted, and the
// wizard concludes identically.
func (_self *OnboardingMachine) SkipTrustPrompt() OnboardingStep {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOnboardingStepINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_skip_trust_prompt(
				_pointer, _uniffiStatus),
		}
	}))
}

func (_self *OnboardingMachine) StartHandleCheck(handle string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_start_handle_check(
			_pointer, FfiConverterStringINSTANCE.Lower(handle)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

}

// Spawns the four-step provisioning orchestrator on the appropriate
// runtime (`tokio::spawn` on native, `wasm_bindgen_futures::spawn_local`
// on web) and returns immediately. All inputs are read from the
// machine's existing state (handle, DNS provider/creds, VPS
// provider/creds, contact, set-up-later flag). Observer ticks drive
// re-render; the client reads `provisioning_snapshot()` on each tick.
//
// Idempotent: safe to call after a partial prior run — the
// orchestrator's pre-flight checks short-circuit completed work.
func (_self *OnboardingMachine) StartProvisioning() {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_start_provisioning(
			_pointer, _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) Step() OnboardingStep {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOnboardingStepINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_step(
				_pointer, _uniffiStatus),
		}
	}))
}

func (_self *OnboardingMachine) SubmitHandleCheckContinue() OnboardingStep {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	res, _ := uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_onboarding_machine_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) OnboardingStep {
			return FfiConverterOnboardingStepINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_submit_handle_check_continue(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_rust_buffer(handle)
		},
	)

	return res
}

// Wire-up for the `nat-mode-confirm-button`. Commits the admin's NAT-axis
// choice via the **mutable** `fauna.setup.nat_mode` kind (pre-identity,
// Ed25519-signed over `mode_wire_str ‖ "\n" ‖ actor_id_hex ‖ "\n" ‖
// timestamp_decimal` — the same signed envelope as the storage-mode
// commit; any valid admin-signed set upserts the `nest_nat_mode` row, no
// conflict reply). On success the wizard exits: state → `Done`,
// `wizard_outcome() == LoggedIn`, returns `OnboardingStep::Done`. On a
// 4xx-class reject the snapshot moves to `Error { transient: false }`; on
// transport/internal failure `Error { transient: true }` — submit stays
// enabled either way (resubmit allowed). Per
// `docs/goal/behavior/onboarding.md` § 3b-bis.
func (_self *OnboardingMachine) SubmitNatModeChoice() OnboardingStep {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	res, _ := uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_onboarding_machine_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) OnboardingStep {
			return FfiConverterOnboardingStepINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_submit_nat_mode_choice(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_rust_buffer(handle)
		},
	)

	return res
}

// `recovery-entry-submit-button` — the phrase-only identity restore
// (`onboarding.md` § 1 Identity; ceremony
// `identity-succession.md` § Seed escrow → *Restore path*).
//
// `kit_input` is `recovery-entry-phrase-field` verbatim (the
// `fauna://recovery` URI or a bare 64-hex code). The account comes from
// [`Self::current_handle`] — the wizard's one account field, which the
// client fills from `recovery-entry-account-field` when the user typed
// one; when it is empty the kit payload's own `handle=` supplies it, and
// this method writes that back so `handle_entry` pre-fills exactly as an
// `identity_import` QR's handle does.
//
// On success the seed is restored and the wizard lands on `HandleEntry` —
// deliberately the same landing `confirm_imported_identity` produces: the
// seed *is* recovered, so everything downstream is an import.
//
// Returns the outcome rather than a message because the machine holds no
// string table; the client resolves the i18n key
// (`onboarding.recovery_entry.*`) and renders it on `error-message`, the
// same division the handle-check's `LocalizedText` snapshots use.
// [`RecoveryEntryOutcome::Superseded`] is the one variant that does not
// stay on the page: the client routes it to
// [`Self::begin_import_identity_with_reason`], uniform with the launch
// flow's superseded refusal.
func (_self *OnboardingMachine) SubmitRecoveryEntry(kitInput string) RecoveryEntryOutcome {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	res, _ := uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_onboarding_machine_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) RecoveryEntryOutcome {
			return FfiConverterRecoveryEntryOutcomeINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_submit_recovery_entry(
			_pointer, FfiConverterStringINSTANCE.Lower(kitInput)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_rust_buffer(handle)
		},
	)

	return res
}

// Consume the pending RecoveryKey root at the wizard's signed-in handoff —
// the one point custody permits registration (the root is never
// persisted, so it cannot survive to a later session). Returns `None` if
// the kit was skipped, already taken, or never offered.
func (_self *OnboardingMachine) TakePendingRecoverySecret() *fauna_core.SecretString {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalSecretStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_take_pending_recovery_secret(
				_pointer, _uniffiStatus),
		}
	}))
}

// Consume the `trust_prompt` answer at the wizard's signed-in handoff —
// the one point the client holds an authenticated session and the nest's
// content-processor roster, i.e. the only place
// `LinkedNestsAction::MintDefaultSet` can run. Consume-once (mirrors
// [`Self::take_pending_recovery_secret`]), so a handoff that runs twice
// mints once. `false` when the user skipped, never asked, or already
// handed off.
func (_self *OnboardingMachine) TakeTrustPromptGranted() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_take_trust_prompt_granted(
			_pointer, _uniffiStatus)
	}))
}

func (_self *OnboardingMachine) ToggleBuyDomain(on bool) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_toggle_buy_domain(
			_pointer, FfiConverterBoolINSTANCE.Lower(on), _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) ToggleSameProviderForVps(on bool) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_toggle_same_provider_for_vps(
			_pointer, FfiConverterBoolINSTANCE.Lower(on), _uniffiStatus)
		return false
	})
}

func (_self *OnboardingMachine) VerifyDns() error {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	_, err := uniffiRustCallAsync[*OnboardingError](
		FfiConverterOnboardingErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_verify_dns(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

	if err == nil {
		return nil
	}

	return err
}

func (_self *OnboardingMachine) VerifyOobInviteCode(code string) {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_verify_oob_invite_code(
			_pointer, FfiConverterStringINSTANCE.Lower(code)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

}

func (_self *OnboardingMachine) VerifyVps() error {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	_, err := uniffiRustCallAsync[*OnboardingError](
		FfiConverterOnboardingErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_onboarding_machine_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_verify_vps(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_void(handle)
		},
	)

	if err == nil {
		return nil
	}

	return err
}

// Fields visible on the dns_config form. Filters by `FieldMeta::kinds`:
// always include kinds containing Dns; additionally include Vps-kinded
// fields when same_provider_for_vps is on AND the provider has Vps cap,
// and Registrar-kinded fields when buy_domain is on AND the provider has
// Registrar cap (e.g. cloudflare's `account-id`, `kinds: [registrar]` —
// without this arm a registrar-only-kinded field never renders, so its
// credential is never collected and the registrar API call it feeds
// fails downstream instead of at the form).
func (_self *OnboardingMachine) VisibleDnsFields() []FieldMetaPlain {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterSequenceFieldMetaPlainINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_visible_dns_fields(
				_pointer, _uniffiStatus),
		}
	}))
}

// Fields visible on the vps_config form. Filters the selected VPS
// provider's fields by `FieldMeta::kinds` containing `Capability::Vps`.
// Mirrors `visible_dns_fields()` so per-app view code is a one-liner
// (`_m.VisibleVpsFields()`) instead of a duplicated client-side filter.
func (_self *OnboardingMachine) VisibleVpsFields() []FieldMetaPlain {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterSequenceFieldMetaPlainINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_visible_vps_fields(
				_pointer, _uniffiStatus),
		}
	}))
}

func (_self *OnboardingMachine) VpsConfig() VpsConfigState {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterVpsConfigStateINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_vps_config(
				_pointer, _uniffiStatus),
		}
	}))
}

// Why `vps-config-continue-button` is dead — `None` when it is live, else
// the one-line explainer naming **the act that revives it**.
//
// **This exists because a disabled control owes the user a reason**
// (`ui/README.md` § Copy comprehensibility rule 5; `apps/tui.md`
// § Rendering → *Control vocabulary* rule 3 restates it for the terminal,
// where DIM is the only other signal). The vps_config page shipped with
// **no** explanatory surface at all: a first-time user landed on four
// provider names, none marked, and a dead Continue, with nothing on screen
// saying what to do — the exact defect [`Self::dns_status_text_key`] was
// split in two to fix one page earlier in the same wizard, found here by
// the walk's wizard driver (`walk.rs`, 2026-08-05).
//
// Paired with [`Self::can_continue_vps`] over one private shortfall, so
// the verdict and its explanation cannot disagree — the
// `dns_provider_eligible` / `dns_provider_ineligible_reason` template. An
// `Option` rather than a "" key on purpose: a blank-string status is
// precisely the bug that left `dns-status-text` painting an empty line in
// its gating state, and `None` makes that unrepresentable.
//
// Four shortfalls, four messages, not one generic line (rule 5 Q2 — the
// user's next act differs in each: pick, verify, choose where, choose how
// big).
func (_self *OnboardingMachine) VpsContinueBlockedReason() *fauna_core.LocalizedText {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalLocalizedTextINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_vps_continue_blocked_reason(
				_pointer, _uniffiStatus),
		}
	}))
}

// Whether the deployment's **files (WebDAV)** subsystem should be enabled
// at claim. Machine-derived sibling of [`Self::carddav_enable_requested`] —
// same two-axis derivation (handle locality AND NAT axis), no checkbox
// (`onboarding.md` § 3b).
//
// The per-app launch glue calls `MailAdminClient::set_webdav_enabled(true)`
// on it at `LoggedIn`. Per `docs/goal/behavior/webdav-server.md`
// § Independent enablement.
func (_self *OnboardingMachine) WebdavEnableRequested() bool {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_webdav_enable_requested(
			_pointer, _uniffiStatus)
	}))
}

func (_self *OnboardingMachine) WizardOutcome() *WizardOutcome {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalWizardOutcomeINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_wizard_outcome(
				_pointer, _uniffiStatus),
		}
	}))
}

// Wire-up for the `claim-code-submit-button`. POSTs the user's
// one-time claim code + Ed25519-signed auth payload to
// `POST /api/v1/claim-admin`. On 2xx, the wizard transitions to
// `Done` with `wizard_outcome() == LoggedIn { nest_url, handle }`;
// on 4xx the wizard stays on `claim_code` with the snapshot moved
// to `Invalid { reason }`; on 5xx / network the snapshot moves to
// `Error { transient: true, ... }`. Per
// `docs/goal/behavior/onboarding.md` §3a.
func (_self *OnboardingMachine) WizardSubmitClaimCode(code string) OnboardingStep {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	res, _ := uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_onboarding_machine_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) OnboardingStep {
			return FfiConverterOnboardingStepINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_wizard_submit_claim_code(
			_pointer, FfiConverterStringINSTANCE.Lower(code)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_rust_buffer(handle)
		},
	)

	return res
}

func (_self *OnboardingMachine) WizardSubmitInviteRequest() OnboardingStep {
	_pointer := _self.ffiObject.incrementPointer("*OnboardingMachine")
	defer _self.ffiObject.decrementPointer()
	res, _ := uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_onboarding_machine_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) OnboardingStep {
			return FfiConverterOnboardingStepINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingmachine_wizard_submit_invite_request(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_onboarding_machine_rust_future_free_rust_buffer(handle)
		},
	)

	return res
}
func (object *OnboardingMachine) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterOnboardingMachine struct{}

var FfiConverterOnboardingMachineINSTANCE = FfiConverterOnboardingMachine{}

func (c FfiConverterOnboardingMachine) Lift(handle C.uint64_t) *OnboardingMachine {
	result := &OnboardingMachine{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_fauna_onboarding_machine_fn_clone_onboardingmachine(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_fauna_onboarding_machine_fn_free_onboardingmachine(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*OnboardingMachine).Destroy)
	return result
}

func (c FfiConverterOnboardingMachine) Read(reader io.Reader) *OnboardingMachine {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterOnboardingMachine) Lower(value *OnboardingMachine) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*OnboardingMachine")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterOnboardingMachine) Write(writer io.Writer, value *OnboardingMachine) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalOnboardingMachine(handle uint64) *OnboardingMachine {
	return FfiConverterOnboardingMachineINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalOnboardingMachine(value *OnboardingMachine) uint64 {
	return uint64(FfiConverterOnboardingMachineINSTANCE.Lower(value))
}

type FfiDestroyerOnboardingMachine struct{}

func (_ FfiDestroyerOnboardingMachine) Destroy(value *OnboardingMachine) {
	value.Destroy()
}

type OnboardingObserver interface {
	// Called whenever the machine's observable state changes. The
	// observer reads a fresh snapshot via the machine's own getter.
	OnChanged()
}
type OnboardingObserverImpl struct {
	ffiObject FfiObject
}

// Called whenever the machine's observable state changes. The
// observer reads a fresh snapshot via the machine's own getter.
func (_self *OnboardingObserverImpl) OnChanged() {
	_pointer := _self.ffiObject.incrementPointer("OnboardingObserver")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_onboarding_machine_fn_method_onboardingobserver_on_changed(
			_pointer, _uniffiStatus)
		return false
	})
}
func (object *OnboardingObserverImpl) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterOnboardingObserver struct {
	handleMap *concurrentHandleMap[OnboardingObserver]
}

var FfiConverterOnboardingObserverINSTANCE = FfiConverterOnboardingObserver{
	handleMap: newConcurrentHandleMap[OnboardingObserver](),
}

func (c FfiConverterOnboardingObserver) Lift(handle C.uint64_t) OnboardingObserver {
	if uint64(handle)&1 == 0 {
		// Rust-generated handle (even), construct a new object wrapping the handle
		result := &OnboardingObserverImpl{
			newFfiObject(
				handle,
				func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
					return C.uniffi_fauna_onboarding_machine_fn_clone_onboardingobserver(handle, status)
				},
				func(handle C.uint64_t, status *C.RustCallStatus) {
					C.uniffi_fauna_onboarding_machine_fn_free_onboardingobserver(handle, status)
				},
			),
		}
		runtime.SetFinalizer(result, (*OnboardingObserverImpl).Destroy)
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

func (c FfiConverterOnboardingObserver) Read(reader io.Reader) OnboardingObserver {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterOnboardingObserver) Lower(value OnboardingObserver) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	if val, ok := value.(*OnboardingObserverImpl); ok {
		// Rust-backed object, clone the handle
		handle := val.ffiObject.incrementPointer("OnboardingObserver")
		defer val.ffiObject.decrementPointer()
		return handle
	} else {
		// Go-backed object, insert into handle map
		return C.uint64_t(c.handleMap.insert(value))
	}
}

func (c FfiConverterOnboardingObserver) Write(writer io.Writer, value OnboardingObserver) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalOnboardingObserver(handle uint64) OnboardingObserver {
	return FfiConverterOnboardingObserverINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalOnboardingObserver(value OnboardingObserver) uint64 {
	return uint64(FfiConverterOnboardingObserverINSTANCE.Lower(value))
}

type FfiDestroyerOnboardingObserver struct{}

func (_ FfiDestroyerOnboardingObserver) Destroy(value OnboardingObserver) {
	if val, ok := value.(*OnboardingObserverImpl); ok {
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

//export fauna_onboarding_machine_observer_cgo_dispatchCallbackInterfaceOnboardingObserverMethod0
func fauna_onboarding_machine_observer_cgo_dispatchCallbackInterfaceOnboardingObserverMethod0(uniffiHandle C.uint64_t, uniffiOutReturn *C.void, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterOnboardingObserverINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	uniffiObj.OnChanged()

}

var UniffiVTableCallbackInterfaceOnboardingObserverINSTANCE = C.UniffiVTableCallbackInterfaceOnboardingObserver{
	uniffiFree:  (C.UniffiCallbackInterfaceFree)(C.fauna_onboarding_machine_observer_cgo_dispatchCallbackInterfaceOnboardingObserverFree),
	uniffiClone: (C.UniffiCallbackInterfaceClone)(C.fauna_onboarding_machine_observer_cgo_dispatchCallbackInterfaceOnboardingObserverClone),
	onChanged:   (C.UniffiCallbackInterfaceOnboardingObserverMethod0)(C.fauna_onboarding_machine_observer_cgo_dispatchCallbackInterfaceOnboardingObserverMethod0),
}

//export fauna_onboarding_machine_observer_cgo_dispatchCallbackInterfaceOnboardingObserverFree
func fauna_onboarding_machine_observer_cgo_dispatchCallbackInterfaceOnboardingObserverFree(handle C.uint64_t) {
	FfiConverterOnboardingObserverINSTANCE.handleMap.remove(uint64(handle))
}

//export fauna_onboarding_machine_observer_cgo_dispatchCallbackInterfaceOnboardingObserverClone
func fauna_onboarding_machine_observer_cgo_dispatchCallbackInterfaceOnboardingObserverClone(handle C.uint64_t) C.uint64_t {
	val, ok := FfiConverterOnboardingObserverINSTANCE.handleMap.tryGet(uint64(handle))
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}
	return C.uint64_t(FfiConverterOnboardingObserverINSTANCE.handleMap.insert(val))
}

func (c FfiConverterOnboardingObserver) register() {
	C.uniffi_fauna_onboarding_machine_fn_init_callback_vtable_onboardingobserver(&UniffiVTableCallbackInterfaceOnboardingObserverINSTANCE)
}

// The platform attestation over `{nest nonce, band, application id, actor}`
// — mirror of `fauna_protocol::age::AgeAttestation`.
type AgeAttestationPlain struct {
	// `"ios"` | `"android"`.
	Platform string
	// The `request_age_nonce` hex the attestation was made over.
	NonceHex string
	// iOS: the App Attest key id (hex); android: `""`.
	KeyIdHex string
	// iOS: the App Attest attestation object; android: the Play Integrity
	// classic-request verdict token (ASCII bytes).
	AttestationObject []byte
}

func (r *AgeAttestationPlain) Destroy() {
	FfiDestroyerString{}.Destroy(r.Platform)
	FfiDestroyerString{}.Destroy(r.NonceHex)
	FfiDestroyerString{}.Destroy(r.KeyIdHex)
	FfiDestroyerBytes{}.Destroy(r.AttestationObject)
}

type FfiConverterAgeAttestationPlain struct{}

var FfiConverterAgeAttestationPlainINSTANCE = FfiConverterAgeAttestationPlain{}

func (c FfiConverterAgeAttestationPlain) Lift(rb RustBufferI) AgeAttestationPlain {
	return LiftFromRustBuffer[AgeAttestationPlain](c, rb)
}

func (c FfiConverterAgeAttestationPlain) Read(reader io.Reader) AgeAttestationPlain {
	return AgeAttestationPlain{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
	}
}

func (c FfiConverterAgeAttestationPlain) Lower(value AgeAttestationPlain) C.RustBuffer {
	return LowerIntoRustBuffer[AgeAttestationPlain](c, value)
}

func (c FfiConverterAgeAttestationPlain) LowerExternal(value AgeAttestationPlain) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AgeAttestationPlain](c, value))
}

func (c FfiConverterAgeAttestationPlain) Write(writer io.Writer, value AgeAttestationPlain) {
	FfiConverterStringINSTANCE.Write(writer, value.Platform)
	FfiConverterStringINSTANCE.Write(writer, value.NonceHex)
	FfiConverterStringINSTANCE.Write(writer, value.KeyIdHex)
	FfiConverterBytesINSTANCE.Write(writer, value.AttestationObject)
}

type FfiDestroyerAgeAttestationPlain struct{}

func (_ FfiDestroyerAgeAttestationPlain) Destroy(value AgeAttestationPlain) {
	value.Destroy()
}

// The app's age claim for the admission it is about to make — the machine
// surface of `fauna_protocol::age::AgeClaim` (`family-safety.md` § The
// account age band). Set by the mobile apps from their store age signal
// (`OnboardingMachine::set_age_claim`); every other app never sets it.
type AgeClaimPlain struct {
	// Wire token: `U13` | `13-15` | `16-17` | `18+`.
	Band string
	// The platform attestation hardening the claim; `None` = declared-only.
	Attestation *AgeAttestationPlain
}

func (r *AgeClaimPlain) Destroy() {
	FfiDestroyerString{}.Destroy(r.Band)
	FfiDestroyerOptionalAgeAttestationPlain{}.Destroy(r.Attestation)
}

type FfiConverterAgeClaimPlain struct{}

var FfiConverterAgeClaimPlainINSTANCE = FfiConverterAgeClaimPlain{}

func (c FfiConverterAgeClaimPlain) Lift(rb RustBufferI) AgeClaimPlain {
	return LiftFromRustBuffer[AgeClaimPlain](c, rb)
}

func (c FfiConverterAgeClaimPlain) Read(reader io.Reader) AgeClaimPlain {
	return AgeClaimPlain{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalAgeAttestationPlainINSTANCE.Read(reader),
	}
}

func (c FfiConverterAgeClaimPlain) Lower(value AgeClaimPlain) C.RustBuffer {
	return LowerIntoRustBuffer[AgeClaimPlain](c, value)
}

func (c FfiConverterAgeClaimPlain) LowerExternal(value AgeClaimPlain) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AgeClaimPlain](c, value))
}

func (c FfiConverterAgeClaimPlain) Write(writer io.Writer, value AgeClaimPlain) {
	FfiConverterStringINSTANCE.Write(writer, value.Band)
	FfiConverterOptionalAgeAttestationPlainINSTANCE.Write(writer, value.Attestation)
}

type FfiDestroyerAgeClaimPlain struct{}

func (_ FfiDestroyerAgeClaimPlain) Destroy(value AgeClaimPlain) {
	value.Destroy()
}

// `request_age_nonce`'s reply — mirror of `nest_api::AgeNonce`.
type AgeNoncePlain struct {
	// 64-char hex of the 32-byte nonce the attestation must bind.
	NonceHex string
	// Seconds the nonce stays redeemable — the attestation round's budget.
	ExpiresInSecs uint64
	// The `AgeAttestationPlain::platform` tokens this nest can verify. The
	// machine sends an attestation only for a platform listed here, over
	// this nonce, to this nest (`family-safety.md` § The account age band →
	// *An attestation the nest cannot check*); the glue may read it to skip
	// a platform round that would be discarded — an economy, never the
	// guarantee. Empty = verifies nothing (an unarmed nest holds no verifier).
	AttestationPlatforms []string
}

func (r *AgeNoncePlain) Destroy() {
	FfiDestroyerString{}.Destroy(r.NonceHex)
	FfiDestroyerUint64{}.Destroy(r.ExpiresInSecs)
	FfiDestroyerSequenceString{}.Destroy(r.AttestationPlatforms)
}

type FfiConverterAgeNoncePlain struct{}

var FfiConverterAgeNoncePlainINSTANCE = FfiConverterAgeNoncePlain{}

func (c FfiConverterAgeNoncePlain) Lift(rb RustBufferI) AgeNoncePlain {
	return LiftFromRustBuffer[AgeNoncePlain](c, rb)
}

func (c FfiConverterAgeNoncePlain) Read(reader io.Reader) AgeNoncePlain {
	return AgeNoncePlain{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterAgeNoncePlain) Lower(value AgeNoncePlain) C.RustBuffer {
	return LowerIntoRustBuffer[AgeNoncePlain](c, value)
}

func (c FfiConverterAgeNoncePlain) LowerExternal(value AgeNoncePlain) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AgeNoncePlain](c, value))
}

func (c FfiConverterAgeNoncePlain) Write(writer io.Writer, value AgeNoncePlain) {
	FfiConverterStringINSTANCE.Write(writer, value.NonceHex)
	FfiConverterUint64INSTANCE.Write(writer, value.ExpiresInSecs)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.AttestationPlatforms)
}

type FfiDestroyerAgeNoncePlain struct{}

func (_ FfiDestroyerAgeNoncePlain) Destroy(value AgeNoncePlain) {
	value.Destroy()
}

type AwaitingManualDnsSnapshot struct {
	State AwaitingDnsState
	// The manual DNS records the user must add at their registrar, surfaced
	// for display. Populated when the surface is entered
	// (`continue_from_dns_post_instructions` / `seed_awaiting_manual_dns`)
	// and preserved across `recheck_manual_dns()` ticks.
	DnsRecords []DnsRecordPlain
	Message    fauna_core.LocalizedText
}

func (r *AwaitingManualDnsSnapshot) Destroy() {
	FfiDestroyerAwaitingDnsState{}.Destroy(r.State)
	FfiDestroyerSequenceDnsRecordPlain{}.Destroy(r.DnsRecords)
	fauna_core.FfiDestroyerLocalizedText{}.Destroy(r.Message)
}

type FfiConverterAwaitingManualDnsSnapshot struct{}

var FfiConverterAwaitingManualDnsSnapshotINSTANCE = FfiConverterAwaitingManualDnsSnapshot{}

func (c FfiConverterAwaitingManualDnsSnapshot) Lift(rb RustBufferI) AwaitingManualDnsSnapshot {
	return LiftFromRustBuffer[AwaitingManualDnsSnapshot](c, rb)
}

func (c FfiConverterAwaitingManualDnsSnapshot) Read(reader io.Reader) AwaitingManualDnsSnapshot {
	return AwaitingManualDnsSnapshot{
		FfiConverterAwaitingDnsStateINSTANCE.Read(reader),
		FfiConverterSequenceDnsRecordPlainINSTANCE.Read(reader),
		fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterAwaitingManualDnsSnapshot) Lower(value AwaitingManualDnsSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[AwaitingManualDnsSnapshot](c, value)
}

func (c FfiConverterAwaitingManualDnsSnapshot) LowerExternal(value AwaitingManualDnsSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AwaitingManualDnsSnapshot](c, value))
}

func (c FfiConverterAwaitingManualDnsSnapshot) Write(writer io.Writer, value AwaitingManualDnsSnapshot) {
	FfiConverterAwaitingDnsStateINSTANCE.Write(writer, value.State)
	FfiConverterSequenceDnsRecordPlainINSTANCE.Write(writer, value.DnsRecords)
	fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, value.Message)
}

type FfiDestroyerAwaitingManualDnsSnapshot struct{}

func (_ FfiDestroyerAwaitingManualDnsSnapshot) Destroy(value AwaitingManualDnsSnapshot) {
	value.Destroy()
}

// One priced line item on the `nest_provisioning` page's top-region price
// summary ("Bill of Materials" — `docs/goal/behavior/onboarding.md` §6).
// `label` reuses the same
// `onboarding.provision.step.{domain,server}` keys the progress rows below
// it already render, so the recap and the step name always agree — see
// [`crate::OnboardingMachine::bill_of_materials`].
type BillOfMaterialsItem struct {
	Label      fauna_core.LocalizedText
	PriceCents uint64
	Currency   string
	// `true` for the VPS's per-month charge, `false` for the domain's
	// one-time registration charge.
	Recurring bool
	// The domain line only: the registrar's per-year renewal price in
	// `currency`, when it quoted one (`RegistrarAvailability::Buyable.
	// renewal_cents`) — rendered as `onboarding.provision.bom_line_domain`
	// ("{price} for the first year, then {renewal}/year"), else the plain
	// `bom_line`. Always `None` on the VPS line. `onboarding.md` § 6.
	RenewalPriceCents *uint64
}

func (r *BillOfMaterialsItem) Destroy() {
	fauna_core.FfiDestroyerLocalizedText{}.Destroy(r.Label)
	FfiDestroyerUint64{}.Destroy(r.PriceCents)
	FfiDestroyerString{}.Destroy(r.Currency)
	FfiDestroyerBool{}.Destroy(r.Recurring)
	FfiDestroyerOptionalUint64{}.Destroy(r.RenewalPriceCents)
}

type FfiConverterBillOfMaterialsItem struct{}

var FfiConverterBillOfMaterialsItemINSTANCE = FfiConverterBillOfMaterialsItem{}

func (c FfiConverterBillOfMaterialsItem) Lift(rb RustBufferI) BillOfMaterialsItem {
	return LiftFromRustBuffer[BillOfMaterialsItem](c, rb)
}

func (c FfiConverterBillOfMaterialsItem) Read(reader io.Reader) BillOfMaterialsItem {
	return BillOfMaterialsItem{
		fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterOptionalUint64INSTANCE.Read(reader),
	}
}

func (c FfiConverterBillOfMaterialsItem) Lower(value BillOfMaterialsItem) C.RustBuffer {
	return LowerIntoRustBuffer[BillOfMaterialsItem](c, value)
}

func (c FfiConverterBillOfMaterialsItem) LowerExternal(value BillOfMaterialsItem) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[BillOfMaterialsItem](c, value))
}

func (c FfiConverterBillOfMaterialsItem) Write(writer io.Writer, value BillOfMaterialsItem) {
	fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, value.Label)
	FfiConverterUint64INSTANCE.Write(writer, value.PriceCents)
	FfiConverterStringINSTANCE.Write(writer, value.Currency)
	FfiConverterBoolINSTANCE.Write(writer, value.Recurring)
	FfiConverterOptionalUint64INSTANCE.Write(writer, value.RenewalPriceCents)
}

type FfiDestroyerBillOfMaterialsItem struct{}

func (_ FfiDestroyerBillOfMaterialsItem) Destroy(value BillOfMaterialsItem) {
	value.Destroy()
}

// The DNS-provider credential the admin verified during onboarding's DNS
// step, exposed by [`crate::OnboardingMachine::captured_dns_credential`] for
// the **launched client** to seal into `fauna.state.dns` via the
// post-onboarding `DnsManagementMachine::PutCredentials` path — one store,
// one writer (`docs/goal/behavior/dns-management.md` § Where the credential
// lives; `docs/goal/behavior/onboarding.md` § 4). The onboarding machine has
// no account-plane write capability and, in the fresh-provision path, no live
// nest "at the end of the DNS step", so it does not seal here; it hands the
// captured credential to the launched client (the onboarding→launch hand-off
// channel).
//
// The shape mirrors the `PutCredentials` inputs: `fields` is the provider
// field-id → value bag (the same `DnsConfigState.creds` map, keyed by
// `providers.yaml` field ids). The covered zones are **not** carried — the
// machine re-runs `verify()` to (re)derive them at seal time, so what
// onboarding captured cannot go stale. The secret field values live only in
// this in-process value and the sealed `fauna.state.dns` row; they are never
// sent to the nest in plaintext (account-plane rows rest client-sealed,
// nest-opaque).
type CapturedDnsCredential struct {
	// `fauna_provisioning::ProviderId::as_str()` (e.g. `"hetzner"`).
	ProviderId string
	// Provider field-id → secret value, keyed by `providers.yaml` field ids.
	// Maps to `DnsAction::PutCredentials.fields` (`Vec<DnsCredentialField>`,
	// whose `value` is the same [`SecretString`]) in the per-app launch glue
	// with no transformation beyond the map→vec shape — the secret stays
	// `SecretString` end-to-end into the downstream `fauna.state.dns` store.
	Fields map[string]fauna_core.SecretString
	// Default user-facing label, `"{provider_id} ({domain})"` — disambiguates
	// multiple held credentials; the credential UI can rename it later.
	Label string
}

func (r *CapturedDnsCredential) Destroy() {
	FfiDestroyerString{}.Destroy(r.ProviderId)
	FfiDestroyerMapStringSecretString{}.Destroy(r.Fields)
	FfiDestroyerString{}.Destroy(r.Label)
}

type FfiConverterCapturedDnsCredential struct{}

var FfiConverterCapturedDnsCredentialINSTANCE = FfiConverterCapturedDnsCredential{}

func (c FfiConverterCapturedDnsCredential) Lift(rb RustBufferI) CapturedDnsCredential {
	return LiftFromRustBuffer[CapturedDnsCredential](c, rb)
}

func (c FfiConverterCapturedDnsCredential) Read(reader io.Reader) CapturedDnsCredential {
	return CapturedDnsCredential{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterMapStringSecretStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterCapturedDnsCredential) Lower(value CapturedDnsCredential) C.RustBuffer {
	return LowerIntoRustBuffer[CapturedDnsCredential](c, value)
}

func (c FfiConverterCapturedDnsCredential) LowerExternal(value CapturedDnsCredential) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[CapturedDnsCredential](c, value))
}

func (c FfiConverterCapturedDnsCredential) Write(writer io.Writer, value CapturedDnsCredential) {
	FfiConverterStringINSTANCE.Write(writer, value.ProviderId)
	FfiConverterMapStringSecretStringINSTANCE.Write(writer, value.Fields)
	FfiConverterStringINSTANCE.Write(writer, value.Label)
}

type FfiDestroyerCapturedDnsCredential struct{}

func (_ FfiDestroyerCapturedDnsCredential) Destroy(value CapturedDnsCredential) {
	value.Destroy()
}

type ClaimCodeSnapshot struct {
	State   ClaimCodeState
	Message fauna_core.LocalizedText
	// True iff the Claim button should be enabled. The machine sets this
	// to false during `Submitting` and true on `Idle` / `Invalid` /
	// `Error`. The client may additionally gate it on input emptiness
	// (UI-side concern) — that's not modelled here.
	SubmitEnabled bool
}

func (r *ClaimCodeSnapshot) Destroy() {
	FfiDestroyerClaimCodeState{}.Destroy(r.State)
	fauna_core.FfiDestroyerLocalizedText{}.Destroy(r.Message)
	FfiDestroyerBool{}.Destroy(r.SubmitEnabled)
}

type FfiConverterClaimCodeSnapshot struct{}

var FfiConverterClaimCodeSnapshotINSTANCE = FfiConverterClaimCodeSnapshot{}

func (c FfiConverterClaimCodeSnapshot) Lift(rb RustBufferI) ClaimCodeSnapshot {
	return LiftFromRustBuffer[ClaimCodeSnapshot](c, rb)
}

func (c FfiConverterClaimCodeSnapshot) Read(reader io.Reader) ClaimCodeSnapshot {
	return ClaimCodeSnapshot{
		FfiConverterClaimCodeStateINSTANCE.Read(reader),
		fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterClaimCodeSnapshot) Lower(value ClaimCodeSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[ClaimCodeSnapshot](c, value)
}

func (c FfiConverterClaimCodeSnapshot) LowerExternal(value ClaimCodeSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ClaimCodeSnapshot](c, value))
}

func (c FfiConverterClaimCodeSnapshot) Write(writer io.Writer, value ClaimCodeSnapshot) {
	FfiConverterClaimCodeStateINSTANCE.Write(writer, value.State)
	fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, value.Message)
	FfiConverterBoolINSTANCE.Write(writer, value.SubmitEnabled)
}

type FfiDestroyerClaimCodeSnapshot struct{}

func (_ FfiDestroyerClaimCodeSnapshot) Destroy(value ClaimCodeSnapshot) {
	value.Destroy()
}

type DnsConfigState struct {
	BuyDomain          bool
	SameProviderForVps bool
	SetUpLater         bool
	SelectedProviderId *string
	// Provider field-id → secret value, captured from the wizard's credential
	// form. The values are [`SecretString`] (zeroized on drop, redacted Debug)
	// so the API token does not linger as a plain `String` for the whole DNS
	// step; serde is transparent (a text string) so the WASM JSON / uniffi
	// marshalling are byte-identical to a plain `String` map.
	Creds    map[string]fauna_core.SecretString
	Verified bool
	ZoneId   *string
	// All zones the registrar reports for the connected account, populated
	// by `verify_dns()`. Used by `provider_status()` to detect the
	// `ProviderHasDomain` case (zone matching `handle_domain()` exists).
	CurrentZones []fauna_provisioning.DnsZone
	// Per-domain availability returned by `Registrar::availability()` when
	// `verify_dns` runs against an unregistered domain on a registrar-
	// capable provider. None means the call wasn't attempted (either no
	// registrar capability or domain isn't `Unregistered`) or it returned
	// `Unavailable` / `TldNotSupported` (treated as "not buyable" for
	// `provider_status()` purposes — see `UnregisteredNotBuyable`).
	CurrentAvailability *fauna_provisioning.RegistrarAvailability
	// WHOIS contact for the buy-domain path. Pre-populated by
	// `verify_dns` via `Registrar::fetch_default_contact()` when the
	// provider's `requires_contact()` is true; user confirms/edits via
	// `set_contact()`. Passed to `provision_with_registration` from
	// `start_provisioning`.
	Contact *fauna_provisioning.ContactInfo
	// Set by `confirm_price()` after the wizard surfaces the registrar's
	// quoted price to the user. `start_provisioning` refuses to advance
	// through the buy-domain path until this is true (so we never charge
	// the user without explicit consent).
	PriceAgreed bool
	// True while `verify_dns()` is in flight. Set to true at the top of
	// the call, cleared on every return path (success or error). The
	// `can_verify_dns()` predicate checks this so the wizard's verify
	// button stays disabled across all 7 apps without per-app
	// in-flight tracking. Not serialized — transient runtime state.
	Verifying bool
	// Per `hosted-auth` field id: where its device-authorization sign-in
	// stands (`OnboardingMachine::hosted_auth_state`; absent = `Idle`). The
	// app paints the field's button label from this
	// (`docs/goal/behavior/onboarding.md` § 4); the token itself lands in
	// [`creds`](Self::creds) under the same field id.
	HostedAuth map[string]HostedAuthState
}

func (r *DnsConfigState) Destroy() {
	FfiDestroyerBool{}.Destroy(r.BuyDomain)
	FfiDestroyerBool{}.Destroy(r.SameProviderForVps)
	FfiDestroyerBool{}.Destroy(r.SetUpLater)
	FfiDestroyerOptionalString{}.Destroy(r.SelectedProviderId)
	FfiDestroyerMapStringSecretString{}.Destroy(r.Creds)
	FfiDestroyerBool{}.Destroy(r.Verified)
	FfiDestroyerOptionalString{}.Destroy(r.ZoneId)
	FfiDestroyerSequenceDnsZone{}.Destroy(r.CurrentZones)
	FfiDestroyerOptionalRegistrarAvailability{}.Destroy(r.CurrentAvailability)
	FfiDestroyerOptionalContactInfo{}.Destroy(r.Contact)
	FfiDestroyerBool{}.Destroy(r.PriceAgreed)
	FfiDestroyerBool{}.Destroy(r.Verifying)
	FfiDestroyerMapStringHostedAuthState{}.Destroy(r.HostedAuth)
}

type FfiConverterDnsConfigState struct{}

var FfiConverterDnsConfigStateINSTANCE = FfiConverterDnsConfigState{}

func (c FfiConverterDnsConfigState) Lift(rb RustBufferI) DnsConfigState {
	return LiftFromRustBuffer[DnsConfigState](c, rb)
}

func (c FfiConverterDnsConfigState) Read(reader io.Reader) DnsConfigState {
	return DnsConfigState{
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterMapStringSecretStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterSequenceDnsZoneINSTANCE.Read(reader),
		FfiConverterOptionalRegistrarAvailabilityINSTANCE.Read(reader),
		FfiConverterOptionalContactInfoINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterMapStringHostedAuthStateINSTANCE.Read(reader),
	}
}

func (c FfiConverterDnsConfigState) Lower(value DnsConfigState) C.RustBuffer {
	return LowerIntoRustBuffer[DnsConfigState](c, value)
}

func (c FfiConverterDnsConfigState) LowerExternal(value DnsConfigState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[DnsConfigState](c, value))
}

func (c FfiConverterDnsConfigState) Write(writer io.Writer, value DnsConfigState) {
	FfiConverterBoolINSTANCE.Write(writer, value.BuyDomain)
	FfiConverterBoolINSTANCE.Write(writer, value.SameProviderForVps)
	FfiConverterBoolINSTANCE.Write(writer, value.SetUpLater)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.SelectedProviderId)
	FfiConverterMapStringSecretStringINSTANCE.Write(writer, value.Creds)
	FfiConverterBoolINSTANCE.Write(writer, value.Verified)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.ZoneId)
	FfiConverterSequenceDnsZoneINSTANCE.Write(writer, value.CurrentZones)
	FfiConverterOptionalRegistrarAvailabilityINSTANCE.Write(writer, value.CurrentAvailability)
	FfiConverterOptionalContactInfoINSTANCE.Write(writer, value.Contact)
	FfiConverterBoolINSTANCE.Write(writer, value.PriceAgreed)
	FfiConverterBoolINSTANCE.Write(writer, value.Verifying)
	FfiConverterMapStringHostedAuthStateINSTANCE.Write(writer, value.HostedAuth)
}

type FfiDestroyerDnsConfigState struct{}

func (_ FfiDestroyerDnsConfigState) Destroy(value DnsConfigState) {
	value.Destroy()
}

// A record the run will remove, or has removed — the view's DNS plan line.
type DnsPlanLine struct {
	Name       string
	RecordType string
	Value      string
	// `true` once the DNS step has walked this line and the zone holds no
	// record matching it — removed by this run, or already gone. A DNS step
	// that fails part way leaves the lines it never reached `false`, which is
	// what lets *delete the server anyway* name exactly what is left.
	Removed bool
}

func (r *DnsPlanLine) Destroy() {
	FfiDestroyerString{}.Destroy(r.Name)
	FfiDestroyerString{}.Destroy(r.RecordType)
	FfiDestroyerString{}.Destroy(r.Value)
	FfiDestroyerBool{}.Destroy(r.Removed)
}

type FfiConverterDnsPlanLine struct{}

var FfiConverterDnsPlanLineINSTANCE = FfiConverterDnsPlanLine{}

func (c FfiConverterDnsPlanLine) Lift(rb RustBufferI) DnsPlanLine {
	return LiftFromRustBuffer[DnsPlanLine](c, rb)
}

func (c FfiConverterDnsPlanLine) Read(reader io.Reader) DnsPlanLine {
	return DnsPlanLine{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterDnsPlanLine) Lower(value DnsPlanLine) C.RustBuffer {
	return LowerIntoRustBuffer[DnsPlanLine](c, value)
}

func (c FfiConverterDnsPlanLine) LowerExternal(value DnsPlanLine) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[DnsPlanLine](c, value))
}

func (c FfiConverterDnsPlanLine) Write(writer io.Writer, value DnsPlanLine) {
	FfiConverterStringINSTANCE.Write(writer, value.Name)
	FfiConverterStringINSTANCE.Write(writer, value.RecordType)
	FfiConverterStringINSTANCE.Write(writer, value.Value)
	FfiConverterBoolINSTANCE.Write(writer, value.Removed)
}

type FfiDestroyerDnsPlanLine struct{}

func (_ FfiDestroyerDnsPlanLine) Destroy(value DnsPlanLine) {
	value.Destroy()
}

// Plain mirror of `fauna_provisioning::dns::DnsRecord` for FFI return from
// `fetch_dkim()`. The native orchestrator type lives in fauna-provisioning;
// this is the same shape but lives in the machine crate so client bindings
// see it as part of the machine's surface.
type DnsRecordPlain struct {
	RecordType string
	Name       string
	Value      string
	Ttl        uint32
	Priority   *uint32
}

func (r *DnsRecordPlain) Destroy() {
	FfiDestroyerString{}.Destroy(r.RecordType)
	FfiDestroyerString{}.Destroy(r.Name)
	FfiDestroyerString{}.Destroy(r.Value)
	FfiDestroyerUint32{}.Destroy(r.Ttl)
	FfiDestroyerOptionalUint32{}.Destroy(r.Priority)
}

type FfiConverterDnsRecordPlain struct{}

var FfiConverterDnsRecordPlainINSTANCE = FfiConverterDnsRecordPlain{}

func (c FfiConverterDnsRecordPlain) Lift(rb RustBufferI) DnsRecordPlain {
	return LiftFromRustBuffer[DnsRecordPlain](c, rb)
}

func (c FfiConverterDnsRecordPlain) Read(reader io.Reader) DnsRecordPlain {
	return DnsRecordPlain{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterOptionalUint32INSTANCE.Read(reader),
	}
}

func (c FfiConverterDnsRecordPlain) Lower(value DnsRecordPlain) C.RustBuffer {
	return LowerIntoRustBuffer[DnsRecordPlain](c, value)
}

func (c FfiConverterDnsRecordPlain) LowerExternal(value DnsRecordPlain) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[DnsRecordPlain](c, value))
}

func (c FfiConverterDnsRecordPlain) Write(writer io.Writer, value DnsRecordPlain) {
	FfiConverterStringINSTANCE.Write(writer, value.RecordType)
	FfiConverterStringINSTANCE.Write(writer, value.Name)
	FfiConverterStringINSTANCE.Write(writer, value.Value)
	FfiConverterUint32INSTANCE.Write(writer, value.Ttl)
	FfiConverterOptionalUint32INSTANCE.Write(writer, value.Priority)
}

type FfiDestroyerDnsRecordPlain struct{}

func (_ FfiDestroyerDnsRecordPlain) Destroy(value DnsRecordPlain) {
	value.Destroy()
}

// FFI-friendly mirror of `fauna_provisioning::providers_generated::FieldMeta`.
// The generated FieldMeta uses `&'static str` which UniFFI can't carry across
// the boundary. This struct is what client bindings see.
type FieldMetaPlain struct {
	Id        string
	FieldType FieldTypePlain
	LabelKey  string
	Required  bool
	Kinds     []CapabilityPlain
}

func (r *FieldMetaPlain) Destroy() {
	FfiDestroyerString{}.Destroy(r.Id)
	FfiDestroyerFieldTypePlain{}.Destroy(r.FieldType)
	FfiDestroyerString{}.Destroy(r.LabelKey)
	FfiDestroyerBool{}.Destroy(r.Required)
	FfiDestroyerSequenceCapabilityPlain{}.Destroy(r.Kinds)
}

type FfiConverterFieldMetaPlain struct{}

var FfiConverterFieldMetaPlainINSTANCE = FfiConverterFieldMetaPlain{}

func (c FfiConverterFieldMetaPlain) Lift(rb RustBufferI) FieldMetaPlain {
	return LiftFromRustBuffer[FieldMetaPlain](c, rb)
}

func (c FfiConverterFieldMetaPlain) Read(reader io.Reader) FieldMetaPlain {
	return FieldMetaPlain{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterFieldTypePlainINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterSequenceCapabilityPlainINSTANCE.Read(reader),
	}
}

func (c FfiConverterFieldMetaPlain) Lower(value FieldMetaPlain) C.RustBuffer {
	return LowerIntoRustBuffer[FieldMetaPlain](c, value)
}

func (c FfiConverterFieldMetaPlain) LowerExternal(value FieldMetaPlain) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[FieldMetaPlain](c, value))
}

func (c FfiConverterFieldMetaPlain) Write(writer io.Writer, value FieldMetaPlain) {
	FfiConverterStringINSTANCE.Write(writer, value.Id)
	FfiConverterFieldTypePlainINSTANCE.Write(writer, value.FieldType)
	FfiConverterStringINSTANCE.Write(writer, value.LabelKey)
	FfiConverterBoolINSTANCE.Write(writer, value.Required)
	FfiConverterSequenceCapabilityPlainINSTANCE.Write(writer, value.Kinds)
}

type FfiDestroyerFieldMetaPlain struct{}

func (_ FfiDestroyerFieldMetaPlain) Destroy(value FieldMetaPlain) {
	value.Destroy()
}

type HandleCheckSnapshot struct {
	Phase                  HandleCheckPhase
	Outcome                HandleCheckOutcome
	Message                fauna_core.LocalizedText
	ContinueEnabled        bool
	ControlCheckboxVisible bool
	ControlCheckboxChecked bool
}

func (r *HandleCheckSnapshot) Destroy() {
	FfiDestroyerHandleCheckPhase{}.Destroy(r.Phase)
	FfiDestroyerHandleCheckOutcome{}.Destroy(r.Outcome)
	fauna_core.FfiDestroyerLocalizedText{}.Destroy(r.Message)
	FfiDestroyerBool{}.Destroy(r.ContinueEnabled)
	FfiDestroyerBool{}.Destroy(r.ControlCheckboxVisible)
	FfiDestroyerBool{}.Destroy(r.ControlCheckboxChecked)
}

type FfiConverterHandleCheckSnapshot struct{}

var FfiConverterHandleCheckSnapshotINSTANCE = FfiConverterHandleCheckSnapshot{}

func (c FfiConverterHandleCheckSnapshot) Lift(rb RustBufferI) HandleCheckSnapshot {
	return LiftFromRustBuffer[HandleCheckSnapshot](c, rb)
}

func (c FfiConverterHandleCheckSnapshot) Read(reader io.Reader) HandleCheckSnapshot {
	return HandleCheckSnapshot{
		FfiConverterHandleCheckPhaseINSTANCE.Read(reader),
		FfiConverterHandleCheckOutcomeINSTANCE.Read(reader),
		fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterHandleCheckSnapshot) Lower(value HandleCheckSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[HandleCheckSnapshot](c, value)
}

func (c FfiConverterHandleCheckSnapshot) LowerExternal(value HandleCheckSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[HandleCheckSnapshot](c, value))
}

func (c FfiConverterHandleCheckSnapshot) Write(writer io.Writer, value HandleCheckSnapshot) {
	FfiConverterHandleCheckPhaseINSTANCE.Write(writer, value.Phase)
	FfiConverterHandleCheckOutcomeINSTANCE.Write(writer, value.Outcome)
	fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, value.Message)
	FfiConverterBoolINSTANCE.Write(writer, value.ContinueEnabled)
	FfiConverterBoolINSTANCE.Write(writer, value.ControlCheckboxVisible)
	FfiConverterBoolINSTANCE.Write(writer, value.ControlCheckboxChecked)
}

type FfiDestroyerHandleCheckSnapshot struct{}

func (_ FfiDestroyerHandleCheckSnapshot) Destroy(value HandleCheckSnapshot) {
	value.Destroy()
}

// What `OnboardingMachine::hosted_auth_begin` hands the app: the URL to open
// through its existing open-URL affordance and the code to show beside it.
type HostedAuthPrompt struct {
	VerificationUrl string
	UserCode        string
}

func (r *HostedAuthPrompt) Destroy() {
	FfiDestroyerString{}.Destroy(r.VerificationUrl)
	FfiDestroyerString{}.Destroy(r.UserCode)
}

type FfiConverterHostedAuthPrompt struct{}

var FfiConverterHostedAuthPromptINSTANCE = FfiConverterHostedAuthPrompt{}

func (c FfiConverterHostedAuthPrompt) Lift(rb RustBufferI) HostedAuthPrompt {
	return LiftFromRustBuffer[HostedAuthPrompt](c, rb)
}

func (c FfiConverterHostedAuthPrompt) Read(reader io.Reader) HostedAuthPrompt {
	return HostedAuthPrompt{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterHostedAuthPrompt) Lower(value HostedAuthPrompt) C.RustBuffer {
	return LowerIntoRustBuffer[HostedAuthPrompt](c, value)
}

func (c FfiConverterHostedAuthPrompt) LowerExternal(value HostedAuthPrompt) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[HostedAuthPrompt](c, value))
}

func (c FfiConverterHostedAuthPrompt) Write(writer io.Writer, value HostedAuthPrompt) {
	FfiConverterStringINSTANCE.Write(writer, value.VerificationUrl)
	FfiConverterStringINSTANCE.Write(writer, value.UserCode)
}

type FfiDestroyerHostedAuthPrompt struct{}

func (_ FfiDestroyerHostedAuthPrompt) Destroy(value HostedAuthPrompt) {
	value.Destroy()
}

// ⚠ Kept deliberately, though no snapshot state carries it any more.
//
// This is a **wire** type: `nest_api::InviteRequestResponse.quota` still
// decodes it, because a nest of any version may send the field and
// `version-compatibility.md` makes evolution additive — a client removing a
// field it merely stopped *using* would refuse payloads it can safely ignore.
// It lost its last snapshot use when `InviteRequestState::Approved` retired
// (2026-08-12). Do not delete it as "unused".
type InviteQuota struct {
	StorageBytes         uint64
	TrafficBytesPerMonth uint64
}

func (r *InviteQuota) Destroy() {
	FfiDestroyerUint64{}.Destroy(r.StorageBytes)
	FfiDestroyerUint64{}.Destroy(r.TrafficBytesPerMonth)
}

type FfiConverterInviteQuota struct{}

var FfiConverterInviteQuotaINSTANCE = FfiConverterInviteQuota{}

func (c FfiConverterInviteQuota) Lift(rb RustBufferI) InviteQuota {
	return LiftFromRustBuffer[InviteQuota](c, rb)
}

func (c FfiConverterInviteQuota) Read(reader io.Reader) InviteQuota {
	return InviteQuota{
		FfiConverterUint64INSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
	}
}

func (c FfiConverterInviteQuota) Lower(value InviteQuota) C.RustBuffer {
	return LowerIntoRustBuffer[InviteQuota](c, value)
}

func (c FfiConverterInviteQuota) LowerExternal(value InviteQuota) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[InviteQuota](c, value))
}

func (c FfiConverterInviteQuota) Write(writer io.Writer, value InviteQuota) {
	FfiConverterUint64INSTANCE.Write(writer, value.StorageBytes)
	FfiConverterUint64INSTANCE.Write(writer, value.TrafficBytesPerMonth)
}

type FfiDestroyerInviteQuota struct{}

func (_ FfiDestroyerInviteQuota) Destroy(value InviteQuota) {
	value.Destroy()
}

type InviteRequestSnapshot struct {
	State              InviteRequestState
	Message            fauna_core.LocalizedText
	ContinueEnabled    bool
	RecheckVisible     bool
	OutOfBandCodeState OobCodeState
	// Localized text for the OOB-code row's status label, derived from
	// `out_of_band_code_state`. Centralized in shared Rust per
	// `docs/goal/behavior/onboarding.md` Architectural rule 4 — clients render
	// `LocalizedText` via their per-platform i18n pipeline; they do NOT
	// recompute the message from the state variant. Refreshed on every
	// `invite_request_snapshot()` read so stale values can't leak.
	OobMessage fauna_core.LocalizedText
	// `invite-request-age-notice` — the store-age round's outcome, shown
	// before submit/redeem so the user knows what the application carries
	// (`family-safety.md` § The account age band, D3; a `platform_elements`
	// entry for android/ios — the five other apps never set a claim and
	// declare the absence). Derived on every `invite_request_snapshot()` read
	// from the claim `set_age_claim` holds
	// ([`fauna_protocol::age::age_notice`]): `None` = the store shared
	// nothing, so the shell renders no notice. Args are nested keys —
	// resolve with `resolve_nested`.
	AgeNotice *fauna_core.LocalizedText
}

func (r *InviteRequestSnapshot) Destroy() {
	FfiDestroyerInviteRequestState{}.Destroy(r.State)
	fauna_core.FfiDestroyerLocalizedText{}.Destroy(r.Message)
	FfiDestroyerBool{}.Destroy(r.ContinueEnabled)
	FfiDestroyerBool{}.Destroy(r.RecheckVisible)
	FfiDestroyerOobCodeState{}.Destroy(r.OutOfBandCodeState)
	fauna_core.FfiDestroyerLocalizedText{}.Destroy(r.OobMessage)
	FfiDestroyerOptionalLocalizedText{}.Destroy(r.AgeNotice)
}

type FfiConverterInviteRequestSnapshot struct{}

var FfiConverterInviteRequestSnapshotINSTANCE = FfiConverterInviteRequestSnapshot{}

func (c FfiConverterInviteRequestSnapshot) Lift(rb RustBufferI) InviteRequestSnapshot {
	return LiftFromRustBuffer[InviteRequestSnapshot](c, rb)
}

func (c FfiConverterInviteRequestSnapshot) Read(reader io.Reader) InviteRequestSnapshot {
	return InviteRequestSnapshot{
		FfiConverterInviteRequestStateINSTANCE.Read(reader),
		fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterOobCodeStateINSTANCE.Read(reader),
		fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterOptionalLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterInviteRequestSnapshot) Lower(value InviteRequestSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[InviteRequestSnapshot](c, value)
}

func (c FfiConverterInviteRequestSnapshot) LowerExternal(value InviteRequestSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[InviteRequestSnapshot](c, value))
}

func (c FfiConverterInviteRequestSnapshot) Write(writer io.Writer, value InviteRequestSnapshot) {
	FfiConverterInviteRequestStateINSTANCE.Write(writer, value.State)
	fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, value.Message)
	FfiConverterBoolINSTANCE.Write(writer, value.ContinueEnabled)
	FfiConverterBoolINSTANCE.Write(writer, value.RecheckVisible)
	FfiConverterOobCodeStateINSTANCE.Write(writer, value.OutOfBandCodeState)
	fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, value.OobMessage)
	FfiConverterOptionalLocalizedTextINSTANCE.Write(writer, value.AgeNotice)
}

type FfiDestroyerInviteRequestSnapshot struct{}

func (_ FfiDestroyerInviteRequestSnapshot) Destroy(value InviteRequestSnapshot) {
	value.Destroy()
}

// A record deliberately left in place, for the person to remove by hand.
//
// `value` is empty where the value is not Fauna's to reconstruct (the `TXT`
// rrsets and the `TLSA`), exactly as [`DnsPlanLine`] leaves the `TLSA`'s.
type LeftoverLine struct {
	Name       string
	RecordType string
	Value      string
	Kind       LeftoverKind
}

func (r *LeftoverLine) Destroy() {
	FfiDestroyerString{}.Destroy(r.Name)
	FfiDestroyerString{}.Destroy(r.RecordType)
	FfiDestroyerString{}.Destroy(r.Value)
	FfiDestroyerLeftoverKind{}.Destroy(r.Kind)
}

type FfiConverterLeftoverLine struct{}

var FfiConverterLeftoverLineINSTANCE = FfiConverterLeftoverLine{}

func (c FfiConverterLeftoverLine) Lift(rb RustBufferI) LeftoverLine {
	return LiftFromRustBuffer[LeftoverLine](c, rb)
}

func (c FfiConverterLeftoverLine) Read(reader io.Reader) LeftoverLine {
	return LeftoverLine{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterLeftoverKindINSTANCE.Read(reader),
	}
}

func (c FfiConverterLeftoverLine) Lower(value LeftoverLine) C.RustBuffer {
	return LowerIntoRustBuffer[LeftoverLine](c, value)
}

func (c FfiConverterLeftoverLine) LowerExternal(value LeftoverLine) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[LeftoverLine](c, value))
}

func (c FfiConverterLeftoverLine) Write(writer io.Writer, value LeftoverLine) {
	FfiConverterStringINSTANCE.Write(writer, value.Name)
	FfiConverterStringINSTANCE.Write(writer, value.RecordType)
	FfiConverterStringINSTANCE.Write(writer, value.Value)
	FfiConverterLeftoverKindINSTANCE.Write(writer, value.Kind)
}

type FfiDestroyerLeftoverLine struct{}

func (_ FfiDestroyerLeftoverLine) Destroy(value LeftoverLine) {
	value.Destroy()
}

// One listed server, as the view renders it.
type ManagedServerRow struct {
	ServerId string
	Name     string
	Ipv4     *string
	// The box's IPv6, when the provider lists one — what scopes the `AAAA`
	// removals, exactly as `ipv4` scopes the `A`.
	Ipv6 *string
	// The attributed domain — **present only when verified** (§ Box → domain
	// attribution). Absent means DNS cleanup is skipped for this row and the
	// transfer-code affordance is gone; the server is still deletable.
	Domain *string
	// Every **other** domain verified to point at this box — the secondary
	// local domains the nest served, whose apex `A` the person's zones or
	// public DNS still resolve to this address (§ DNS cleanup — several
	// domains). Each is planned, or listed by hand, beside the attributed
	// domain; never a transfer-code target. Empty when the box has no IPv4.
	SecondaryDomains []string
	// `false` only where the provider has no label facility (OVH), so the row
	// could not be proven fauna-provisioned. The view shows an unmarked note.
	Marked bool
	// This session's own nest — the box the app passed an address for.
	Current      bool
	TransferCode TransferCodeState
}

func (r *ManagedServerRow) Destroy() {
	FfiDestroyerString{}.Destroy(r.ServerId)
	FfiDestroyerString{}.Destroy(r.Name)
	FfiDestroyerOptionalString{}.Destroy(r.Ipv4)
	FfiDestroyerOptionalString{}.Destroy(r.Ipv6)
	FfiDestroyerOptionalString{}.Destroy(r.Domain)
	FfiDestroyerSequenceString{}.Destroy(r.SecondaryDomains)
	FfiDestroyerBool{}.Destroy(r.Marked)
	FfiDestroyerBool{}.Destroy(r.Current)
	FfiDestroyerTransferCodeState{}.Destroy(r.TransferCode)
}

type FfiConverterManagedServerRow struct{}

var FfiConverterManagedServerRowINSTANCE = FfiConverterManagedServerRow{}

func (c FfiConverterManagedServerRow) Lift(rb RustBufferI) ManagedServerRow {
	return LiftFromRustBuffer[ManagedServerRow](c, rb)
}

func (c FfiConverterManagedServerRow) Read(reader io.Reader) ManagedServerRow {
	return ManagedServerRow{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterTransferCodeStateINSTANCE.Read(reader),
	}
}

func (c FfiConverterManagedServerRow) Lower(value ManagedServerRow) C.RustBuffer {
	return LowerIntoRustBuffer[ManagedServerRow](c, value)
}

func (c FfiConverterManagedServerRow) LowerExternal(value ManagedServerRow) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ManagedServerRow](c, value))
}

func (c FfiConverterManagedServerRow) Write(writer io.Writer, value ManagedServerRow) {
	FfiConverterStringINSTANCE.Write(writer, value.ServerId)
	FfiConverterStringINSTANCE.Write(writer, value.Name)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Ipv4)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Ipv6)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Domain)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.SecondaryDomains)
	FfiConverterBoolINSTANCE.Write(writer, value.Marked)
	FfiConverterBoolINSTANCE.Write(writer, value.Current)
	FfiConverterTransferCodeStateINSTANCE.Write(writer, value.TransferCode)
}

type FfiDestroyerManagedServerRow struct{}

func (_ FfiDestroyerManagedServerRow) Destroy(value ManagedServerRow) {
	value.Destroy()
}

type NatModeSnapshot struct {
	State NatModeState
	// The mode the confirm button will commit. Defaults to the nest's
	// resolved seed (read from `fauna.setup.status`'s `node_mode`), refined
	// private-ward for a private-network handle target — see
	// `OnboardingMachine::reset_nat_mode_snapshot`.
	SelectedMode fauna_core.NodeMode
	Message      fauna_core.LocalizedText
	// True iff `nat-mode-confirm-button` should be enabled: `Choosing` and
	// `Error` (resubmit allowed); disabled in `Submitting` and `Done`.
	SubmitEnabled bool
}

func (r *NatModeSnapshot) Destroy() {
	FfiDestroyerNatModeState{}.Destroy(r.State)
	fauna_core.FfiDestroyerNodeMode{}.Destroy(r.SelectedMode)
	fauna_core.FfiDestroyerLocalizedText{}.Destroy(r.Message)
	FfiDestroyerBool{}.Destroy(r.SubmitEnabled)
}

type FfiConverterNatModeSnapshot struct{}

var FfiConverterNatModeSnapshotINSTANCE = FfiConverterNatModeSnapshot{}

func (c FfiConverterNatModeSnapshot) Lift(rb RustBufferI) NatModeSnapshot {
	return LiftFromRustBuffer[NatModeSnapshot](c, rb)
}

func (c FfiConverterNatModeSnapshot) Read(reader io.Reader) NatModeSnapshot {
	return NatModeSnapshot{
		FfiConverterNatModeStateINSTANCE.Read(reader),
		fauna_core.FfiConverterNodeModeINSTANCE.Read(reader),
		fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterNatModeSnapshot) Lower(value NatModeSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[NatModeSnapshot](c, value)
}

func (c FfiConverterNatModeSnapshot) LowerExternal(value NatModeSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[NatModeSnapshot](c, value))
}

func (c FfiConverterNatModeSnapshot) Write(writer io.Writer, value NatModeSnapshot) {
	FfiConverterNatModeStateINSTANCE.Write(writer, value.State)
	fauna_core.FfiConverterNodeModeINSTANCE.Write(writer, value.SelectedMode)
	fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, value.Message)
	FfiConverterBoolINSTANCE.Write(writer, value.SubmitEnabled)
}

type FfiDestroyerNatModeSnapshot struct{}

func (_ FfiDestroyerNatModeSnapshot) Destroy(value NatModeSnapshot) {
	value.Destroy()
}

// Everything an app needs to write the long-term store's pending-invite slot,
// assembled once here instead of seven times per app.
//
// This is the seam that replaced `WizardOutcome::InviteSubmitted` (retired
// 2026-08-12, `onboarding.md` § Wizard exit handling). The pending-review
// journey no longer *exits* the wizard, so there is no outcome to key the
// write on; the app reads this at the `wizard_submit_invite_request()` return
// instead — "the only write moment" (`onboarding.md` § 3 Persistence
// callouts).
//
// Two rules live in here rather than in each app, because getting either wrong
// is silent:
// - **`nest_url` is `state.nest_url`, never `effective_nest_url()`.** The
// `provider_base_urls` override retargets HTTP requests only; leaking it here
// would write a test-cloud URL into a production identity-store record.
// - **`status_json` is the serialized `InviteRequestState`,** opaque to the
// app (`onboarding.md` § Long-term store contract: "Don't validate the
// JSON"). A serialization failure degrades to an empty string so the wizard
// falls back to `PendingReview` on reseed rather than stranding the request.
type PendingInviteSlot struct {
	NestUrl    string
	Handle     string
	RequestId  string
	StatusJson string
}

func (r *PendingInviteSlot) Destroy() {
	FfiDestroyerString{}.Destroy(r.NestUrl)
	FfiDestroyerString{}.Destroy(r.Handle)
	FfiDestroyerString{}.Destroy(r.RequestId)
	FfiDestroyerString{}.Destroy(r.StatusJson)
}

type FfiConverterPendingInviteSlot struct{}

var FfiConverterPendingInviteSlotINSTANCE = FfiConverterPendingInviteSlot{}

func (c FfiConverterPendingInviteSlot) Lift(rb RustBufferI) PendingInviteSlot {
	return LiftFromRustBuffer[PendingInviteSlot](c, rb)
}

func (c FfiConverterPendingInviteSlot) Read(reader io.Reader) PendingInviteSlot {
	return PendingInviteSlot{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterPendingInviteSlot) Lower(value PendingInviteSlot) C.RustBuffer {
	return LowerIntoRustBuffer[PendingInviteSlot](c, value)
}

func (c FfiConverterPendingInviteSlot) LowerExternal(value PendingInviteSlot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[PendingInviteSlot](c, value))
}

func (c FfiConverterPendingInviteSlot) Write(writer io.Writer, value PendingInviteSlot) {
	FfiConverterStringINSTANCE.Write(writer, value.NestUrl)
	FfiConverterStringINSTANCE.Write(writer, value.Handle)
	FfiConverterStringINSTANCE.Write(writer, value.RequestId)
	FfiConverterStringINSTANCE.Write(writer, value.StatusJson)
}

type FfiDestroyerPendingInviteSlot struct{}

func (_ FfiDestroyerPendingInviteSlot) Destroy(value PendingInviteSlot) {
	value.Destroy()
}

// One predecessor identity recovered from the escrow blob's additive section.
type RestoredPredecessorSeed struct {
	// The predecessor's 64-hex actor id — which corpus this seed opens.
	ActorIdHex string
	// That identity's seed, 64-hex — a full root identity seed opening a
	// whole sealed corpus, same custody rule as `State::generated_secret`/
	// `imported_secret` (`key-material-hierarchy.md` § Carrier shape: this
	// field is the third member of that family, held here rather than
	// dropped after read only because the client has not yet persisted it).
	// Held as [`SecretString`] (zeroize-on-drop + redacted `Debug`). Wire
	// unchanged: `SecretString` serializes identically to `String`.
	SeedHex fauna_core.SecretString
}

func (r *RestoredPredecessorSeed) Destroy() {
	FfiDestroyerString{}.Destroy(r.ActorIdHex)
	fauna_core.FfiDestroyerTypeSecretString{}.Destroy(r.SeedHex)
}

type FfiConverterRestoredPredecessorSeed struct{}

var FfiConverterRestoredPredecessorSeedINSTANCE = FfiConverterRestoredPredecessorSeed{}

func (c FfiConverterRestoredPredecessorSeed) Lift(rb RustBufferI) RestoredPredecessorSeed {
	return LiftFromRustBuffer[RestoredPredecessorSeed](c, rb)
}

func (c FfiConverterRestoredPredecessorSeed) Read(reader io.Reader) RestoredPredecessorSeed {
	return RestoredPredecessorSeed{
		FfiConverterStringINSTANCE.Read(reader),
		fauna_core.FfiConverterTypeSecretStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterRestoredPredecessorSeed) Lower(value RestoredPredecessorSeed) C.RustBuffer {
	return LowerIntoRustBuffer[RestoredPredecessorSeed](c, value)
}

func (c FfiConverterRestoredPredecessorSeed) LowerExternal(value RestoredPredecessorSeed) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RestoredPredecessorSeed](c, value))
}

func (c FfiConverterRestoredPredecessorSeed) Write(writer io.Writer, value RestoredPredecessorSeed) {
	FfiConverterStringINSTANCE.Write(writer, value.ActorIdHex)
	fauna_core.FfiConverterTypeSecretStringINSTANCE.Write(writer, value.SeedHex)
}

type FfiDestroyerRestoredPredecessorSeed struct{}

func (_ FfiDestroyerRestoredPredecessorSeed) Destroy(value RestoredPredecessorSeed) {
	value.Destroy()
}

type RetireSnapshot struct {
	Phase   RetirePhase
	Servers []ManagedServerRow
	// The selected row's `server_id`.
	Selected *string
	Steps    []RetireStepRow
	// What the DNS step will remove — shown in the confirm summary so the
	// person sees the plan before agreeing to it.
	DnsPlan         []DnsPlanLine
	LeftoverRecords []LeftoverLine
	// What the person has typed into the confirm field.
	ConfirmName string
	// `true` once `confirm_name` exactly matches the selected server's name.
	ConfirmEnabled bool
	// Whether *delete the server anyway* is offered — a failed DNS step only.
	ForceServerOffered bool
	Error              *string
	// The row a finished run deleted — it has left `servers` by then, and
	// the done state still names it (and says whether it was this session's
	// own box, which is what sends the app to its launch flow).
	Retired *ManagedServerRow
}

func (r *RetireSnapshot) Destroy() {
	FfiDestroyerRetirePhase{}.Destroy(r.Phase)
	FfiDestroyerSequenceManagedServerRow{}.Destroy(r.Servers)
	FfiDestroyerOptionalString{}.Destroy(r.Selected)
	FfiDestroyerSequenceRetireStepRow{}.Destroy(r.Steps)
	FfiDestroyerSequenceDnsPlanLine{}.Destroy(r.DnsPlan)
	FfiDestroyerSequenceLeftoverLine{}.Destroy(r.LeftoverRecords)
	FfiDestroyerString{}.Destroy(r.ConfirmName)
	FfiDestroyerBool{}.Destroy(r.ConfirmEnabled)
	FfiDestroyerBool{}.Destroy(r.ForceServerOffered)
	FfiDestroyerOptionalString{}.Destroy(r.Error)
	FfiDestroyerOptionalManagedServerRow{}.Destroy(r.Retired)
}

type FfiConverterRetireSnapshot struct{}

var FfiConverterRetireSnapshotINSTANCE = FfiConverterRetireSnapshot{}

func (c FfiConverterRetireSnapshot) Lift(rb RustBufferI) RetireSnapshot {
	return LiftFromRustBuffer[RetireSnapshot](c, rb)
}

func (c FfiConverterRetireSnapshot) Read(reader io.Reader) RetireSnapshot {
	return RetireSnapshot{
		FfiConverterRetirePhaseINSTANCE.Read(reader),
		FfiConverterSequenceManagedServerRowINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterSequenceRetireStepRowINSTANCE.Read(reader),
		FfiConverterSequenceDnsPlanLineINSTANCE.Read(reader),
		FfiConverterSequenceLeftoverLineINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalManagedServerRowINSTANCE.Read(reader),
	}
}

func (c FfiConverterRetireSnapshot) Lower(value RetireSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[RetireSnapshot](c, value)
}

func (c FfiConverterRetireSnapshot) LowerExternal(value RetireSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RetireSnapshot](c, value))
}

func (c FfiConverterRetireSnapshot) Write(writer io.Writer, value RetireSnapshot) {
	FfiConverterRetirePhaseINSTANCE.Write(writer, value.Phase)
	FfiConverterSequenceManagedServerRowINSTANCE.Write(writer, value.Servers)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Selected)
	FfiConverterSequenceRetireStepRowINSTANCE.Write(writer, value.Steps)
	FfiConverterSequenceDnsPlanLineINSTANCE.Write(writer, value.DnsPlan)
	FfiConverterSequenceLeftoverLineINSTANCE.Write(writer, value.LeftoverRecords)
	FfiConverterStringINSTANCE.Write(writer, value.ConfirmName)
	FfiConverterBoolINSTANCE.Write(writer, value.ConfirmEnabled)
	FfiConverterBoolINSTANCE.Write(writer, value.ForceServerOffered)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Error)
	FfiConverterOptionalManagedServerRowINSTANCE.Write(writer, value.Retired)
}

type FfiDestroyerRetireSnapshot struct{}

func (_ FfiDestroyerRetireSnapshot) Destroy(value RetireSnapshot) {
	value.Destroy()
}

type RetireStepRow struct {
	Step  RetireStep
	State StepState
}

func (r *RetireStepRow) Destroy() {
	FfiDestroyerRetireStep{}.Destroy(r.Step)
	FfiDestroyerStepState{}.Destroy(r.State)
}

type FfiConverterRetireStepRow struct{}

var FfiConverterRetireStepRowINSTANCE = FfiConverterRetireStepRow{}

func (c FfiConverterRetireStepRow) Lift(rb RustBufferI) RetireStepRow {
	return LiftFromRustBuffer[RetireStepRow](c, rb)
}

func (c FfiConverterRetireStepRow) Read(reader io.Reader) RetireStepRow {
	return RetireStepRow{
		FfiConverterRetireStepINSTANCE.Read(reader),
		FfiConverterStepStateINSTANCE.Read(reader),
	}
}

func (c FfiConverterRetireStepRow) Lower(value RetireStepRow) C.RustBuffer {
	return LowerIntoRustBuffer[RetireStepRow](c, value)
}

func (c FfiConverterRetireStepRow) LowerExternal(value RetireStepRow) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RetireStepRow](c, value))
}

func (c FfiConverterRetireStepRow) Write(writer io.Writer, value RetireStepRow) {
	FfiConverterRetireStepINSTANCE.Write(writer, value.Step)
	FfiConverterStepStateINSTANCE.Write(writer, value.State)
}

type FfiDestroyerRetireStepRow struct{}

func (_ FfiDestroyerRetireStepRow) Destroy(value RetireStepRow) {
	value.Destroy()
}

// Wire shape of `GET /api/v1/setup-status`. The endpoint is
// unauthenticated and returns `{"claimed": bool, "node_mode": "..."}`.
//
// `Default` = the fresh-unclaimed-nest shape (`claimed: false`, node mode
// unset); fixtures grow via struct-update (`..Default::default()`) rather
// than hand-listing every field, so two branches that independently add a
// field merge cleanly instead of colliding on the grown axis.
type SetupStatus struct {
	Claimed bool
	// The nest's resolved NAT axis (`public` / `private`) — the client-set
	// `nest_nat_mode` row falling back to the `FAUNA_MODE` seed. `None` until
	// the setup-status probe has resolved. Seeds the `nat_mode_choice` pre-selection
	// (`docs/goal/behavior/onboarding.md` § 3b-bis).
	NodeMode *fauna_core.NodeMode
}

func (r *SetupStatus) Destroy() {
	FfiDestroyerBool{}.Destroy(r.Claimed)
	FfiDestroyerOptionalNodeMode{}.Destroy(r.NodeMode)
}

type FfiConverterSetupStatus struct{}

var FfiConverterSetupStatusINSTANCE = FfiConverterSetupStatus{}

func (c FfiConverterSetupStatus) Lift(rb RustBufferI) SetupStatus {
	return LiftFromRustBuffer[SetupStatus](c, rb)
}

func (c FfiConverterSetupStatus) Read(reader io.Reader) SetupStatus {
	return SetupStatus{
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterOptionalNodeModeINSTANCE.Read(reader),
	}
}

func (c FfiConverterSetupStatus) Lower(value SetupStatus) C.RustBuffer {
	return LowerIntoRustBuffer[SetupStatus](c, value)
}

func (c FfiConverterSetupStatus) LowerExternal(value SetupStatus) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[SetupStatus](c, value))
}

func (c FfiConverterSetupStatus) Write(writer io.Writer, value SetupStatus) {
	FfiConverterBoolINSTANCE.Write(writer, value.Claimed)
	FfiConverterOptionalNodeModeINSTANCE.Write(writer, value.NodeMode)
}

type FfiDestroyerSetupStatus struct{}

func (_ FfiDestroyerSetupStatus) Destroy(value SetupStatus) {
	value.Destroy()
}

type VpsConfigState struct {
	SelectedProviderId *string
	// Provider field-id → secret value (the VPS-provider API token/keys). Same
	// [`SecretString`] discipline as [`DnsConfigState::creds`].
	Creds                map[string]fauna_core.SecretString
	Verified             bool
	ServerTypes          []fauna_provisioning.ServerTypeInfo
	SelectedServerTypeId *string
	Locations            []fauna_provisioning.VpsLocation
	SelectedLocationId   *string
	// Mail-vs-social intent for the box, set by the `vps-config-mail-mode-toggle`.
	// `None` = "not chosen — use the handle's real-domain default"
	// (`OnboardingMachine::provision_mail_mode_enabled`); `Some(true)` = mail box
	// (scanner sidecars + mail ports, needs `mem_gb ≥ 2`), `Some(false)` =
	// social-only box (lean nest+watchtower compose, the 1 GB tier). Feeds
	// `CloudInitParams::enable_mail` via `run_provisioning_inner`.
	// `docs/goal/behavior/onboarding.md` §5.
	EnableMail *bool
	// Which builds the box's updater follows, set by the
	// `vps-config-update-channel-row` radios. `None` = "not chosen — the
	// default channel" (`OnboardingMachine::provision_update_channel`). Feeds
	// `CloudInitParams::image_tag` via `run_provisioning_inner`.
	// `docs/goal/behavior/onboarding-provisioning.md` §5.
	UpdateChannel *fauna_provisioning.UpdateChannel
	// True while `verify_vps()` is in flight. See the matching field on
	// `DnsConfigState` for rationale. Not serialized.
	Verifying bool
	// Per `hosted-auth` field id: its sign-in state on the VPS form — the
	// twin of [`DnsConfigState::hosted_auth`], mirrored from it by
	// `continue_from_dns` when the same provider serves both.
	HostedAuth map[string]HostedAuthState
}

func (r *VpsConfigState) Destroy() {
	FfiDestroyerOptionalString{}.Destroy(r.SelectedProviderId)
	FfiDestroyerMapStringSecretString{}.Destroy(r.Creds)
	FfiDestroyerBool{}.Destroy(r.Verified)
	FfiDestroyerSequenceServerTypeInfo{}.Destroy(r.ServerTypes)
	FfiDestroyerOptionalString{}.Destroy(r.SelectedServerTypeId)
	FfiDestroyerSequenceVpsLocation{}.Destroy(r.Locations)
	FfiDestroyerOptionalString{}.Destroy(r.SelectedLocationId)
	FfiDestroyerOptionalBool{}.Destroy(r.EnableMail)
	FfiDestroyerOptionalUpdateChannel{}.Destroy(r.UpdateChannel)
	FfiDestroyerBool{}.Destroy(r.Verifying)
	FfiDestroyerMapStringHostedAuthState{}.Destroy(r.HostedAuth)
}

type FfiConverterVpsConfigState struct{}

var FfiConverterVpsConfigStateINSTANCE = FfiConverterVpsConfigState{}

func (c FfiConverterVpsConfigState) Lift(rb RustBufferI) VpsConfigState {
	return LiftFromRustBuffer[VpsConfigState](c, rb)
}

func (c FfiConverterVpsConfigState) Read(reader io.Reader) VpsConfigState {
	return VpsConfigState{
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterMapStringSecretStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterSequenceServerTypeInfoINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterSequenceVpsLocationINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalBoolINSTANCE.Read(reader),
		FfiConverterOptionalUpdateChannelINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterMapStringHostedAuthStateINSTANCE.Read(reader),
	}
}

func (c FfiConverterVpsConfigState) Lower(value VpsConfigState) C.RustBuffer {
	return LowerIntoRustBuffer[VpsConfigState](c, value)
}

func (c FfiConverterVpsConfigState) LowerExternal(value VpsConfigState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[VpsConfigState](c, value))
}

func (c FfiConverterVpsConfigState) Write(writer io.Writer, value VpsConfigState) {
	FfiConverterOptionalStringINSTANCE.Write(writer, value.SelectedProviderId)
	FfiConverterMapStringSecretStringINSTANCE.Write(writer, value.Creds)
	FfiConverterBoolINSTANCE.Write(writer, value.Verified)
	FfiConverterSequenceServerTypeInfoINSTANCE.Write(writer, value.ServerTypes)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.SelectedServerTypeId)
	FfiConverterSequenceVpsLocationINSTANCE.Write(writer, value.Locations)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.SelectedLocationId)
	FfiConverterOptionalBoolINSTANCE.Write(writer, value.EnableMail)
	FfiConverterOptionalUpdateChannelINSTANCE.Write(writer, value.UpdateChannel)
	FfiConverterBoolINSTANCE.Write(writer, value.Verifying)
	FfiConverterMapStringHostedAuthStateINSTANCE.Write(writer, value.HostedAuth)
}

type FfiDestroyerVpsConfigState struct{}

func (_ FfiDestroyerVpsConfigState) Destroy(value VpsConfigState) {
	value.Destroy()
}

type AwaitingDnsState interface {
	Destroy()
}

// Resting state. The user has been shown the DNS records; the next
// `recheck_manual_dns()` will probe. Also the landing state after a
// *transient* probe/claim failure (nest not reachable yet, or a 5xx
// while claiming) — the client keeps polling.
type AwaitingDnsStatePending struct {
}

func (e AwaitingDnsStatePending) Destroy() {
}

// A reachability probe (`fauna.setup.status`) is in flight.
type AwaitingDnsStateChecking struct {
}

func (e AwaitingDnsStateChecking) Destroy() {
}

// The nest is reachable and unclaimed; the claim
// (`fauna.auth.claim_admin`) is in flight.
type AwaitingDnsStateClaiming struct {
}

func (e AwaitingDnsStateClaiming) Destroy() {
}

// The claim succeeded (or the nest was already claimed). The machine
// transitions to `NatModeChoice` (fresh claim) or sets
// `wizard_outcome()` to `LoggedIn` (already-claimed resume); this state
// is set briefly before the surface changes.
type AwaitingDnsStateClaimed struct {
}

func (e AwaitingDnsStateClaimed) Destroy() {
}

// Terminal failure: the claim was rejected (bad/expired claim code), the
// local identity is corrupt, or a probe/claim connection failed
// first-contact identity verification against the held root
// (`security.md` § Pre-claim surfacing — a mismatch on a box this client
// provisioned is MITM/bug, never "still waiting for DNS"). Transient
// failures do *not* land here — they return to `Pending` so polling
// continues.
type AwaitingDnsStateError struct {
	Cause string
}

func (e AwaitingDnsStateError) Destroy() {
	FfiDestroyerString{}.Destroy(e.Cause)
}

type FfiConverterAwaitingDnsState struct{}

var FfiConverterAwaitingDnsStateINSTANCE = FfiConverterAwaitingDnsState{}

func (c FfiConverterAwaitingDnsState) Lift(rb RustBufferI) AwaitingDnsState {
	return LiftFromRustBuffer[AwaitingDnsState](c, rb)
}

func (c FfiConverterAwaitingDnsState) Lower(value AwaitingDnsState) C.RustBuffer {
	return LowerIntoRustBuffer[AwaitingDnsState](c, value)
}

func (c FfiConverterAwaitingDnsState) LowerExternal(value AwaitingDnsState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AwaitingDnsState](c, value))
}
func (FfiConverterAwaitingDnsState) Read(reader io.Reader) AwaitingDnsState {
	id := readInt32(reader)
	switch id {
	case 1:
		return AwaitingDnsStatePending{}
	case 2:
		return AwaitingDnsStateChecking{}
	case 3:
		return AwaitingDnsStateClaiming{}
	case 4:
		return AwaitingDnsStateClaimed{}
	case 5:
		return AwaitingDnsStateError{
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterAwaitingDnsState.Read()", id))
	}
}

func (FfiConverterAwaitingDnsState) Write(writer io.Writer, value AwaitingDnsState) {
	switch variant_value := value.(type) {
	case AwaitingDnsStatePending:
		writeInt32(writer, 1)
	case AwaitingDnsStateChecking:
		writeInt32(writer, 2)
	case AwaitingDnsStateClaiming:
		writeInt32(writer, 3)
	case AwaitingDnsStateClaimed:
		writeInt32(writer, 4)
	case AwaitingDnsStateError:
		writeInt32(writer, 5)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Cause)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterAwaitingDnsState.Write", value))
	}
}

type FfiDestroyerAwaitingDnsState struct{}

func (_ FfiDestroyerAwaitingDnsState) Destroy(value AwaitingDnsState) {
	value.Destroy()
}

// Which entry the total-box-loss recovery wizard branch was reached from
// (`box-recovery.md` § Recovery UI, step 4). Determines where
// `recover-back-button` returns to and lets the per-app glue own the
// launch↔wizard boundary the machine can't represent (`launch_retry` is a
// launch-flow surface, not an `OnboardingStep`).
type BoxRecoveryEntry uint

const (
	// `launch-recover-button` on `launch_retry` (surviving device). Back exits
	// the wizard to the launch retry surface — glue-owned.
	BoxRecoveryEntryLaunch BoxRecoveryEntry = 1
	// `recover-lost-box-button` on `identity_choice` → `identity_import`. Back
	// returns to `identity_import`.
	BoxRecoveryEntryIdentity BoxRecoveryEntry = 2
)

type FfiConverterBoxRecoveryEntry struct{}

var FfiConverterBoxRecoveryEntryINSTANCE = FfiConverterBoxRecoveryEntry{}

func (c FfiConverterBoxRecoveryEntry) Lift(rb RustBufferI) BoxRecoveryEntry {
	return LiftFromRustBuffer[BoxRecoveryEntry](c, rb)
}

func (c FfiConverterBoxRecoveryEntry) Lower(value BoxRecoveryEntry) C.RustBuffer {
	return LowerIntoRustBuffer[BoxRecoveryEntry](c, value)
}

func (c FfiConverterBoxRecoveryEntry) LowerExternal(value BoxRecoveryEntry) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[BoxRecoveryEntry](c, value))
}
func (FfiConverterBoxRecoveryEntry) Read(reader io.Reader) BoxRecoveryEntry {
	id := readInt32(reader)
	return BoxRecoveryEntry(id)
}

func (FfiConverterBoxRecoveryEntry) Write(writer io.Writer, value BoxRecoveryEntry) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerBoxRecoveryEntry struct{}

func (_ FfiDestroyerBoxRecoveryEntry) Destroy(value BoxRecoveryEntry) {
}

type CapabilityPlain uint

const (
	CapabilityPlainDns       CapabilityPlain = 1
	CapabilityPlainVps       CapabilityPlain = 2
	CapabilityPlainRegistrar CapabilityPlain = 3
)

type FfiConverterCapabilityPlain struct{}

var FfiConverterCapabilityPlainINSTANCE = FfiConverterCapabilityPlain{}

func (c FfiConverterCapabilityPlain) Lift(rb RustBufferI) CapabilityPlain {
	return LiftFromRustBuffer[CapabilityPlain](c, rb)
}

func (c FfiConverterCapabilityPlain) Lower(value CapabilityPlain) C.RustBuffer {
	return LowerIntoRustBuffer[CapabilityPlain](c, value)
}

func (c FfiConverterCapabilityPlain) LowerExternal(value CapabilityPlain) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[CapabilityPlain](c, value))
}
func (FfiConverterCapabilityPlain) Read(reader io.Reader) CapabilityPlain {
	id := readInt32(reader)
	return CapabilityPlain(id)
}

func (FfiConverterCapabilityPlain) Write(writer io.Writer, value CapabilityPlain) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerCapabilityPlain struct{}

func (_ FfiDestroyerCapabilityPlain) Destroy(value CapabilityPlain) {
}

type ClaimCodeState interface {
	Destroy()
}

// Initial state. Submit-enabled iff input is non-empty (the
// `submit_enabled` field on the snapshot — driven from machine state).
type ClaimCodeStateIdle struct {
}

func (e ClaimCodeStateIdle) Destroy() {
}

// `wizard_submit_claim_code` is in flight. Submit disabled.
type ClaimCodeStateSubmitting struct {
}

func (e ClaimCodeStateSubmitting) Destroy() {
}

// Server returned 2xx. The wizard transitions to `Done` and
// `wizard_outcome()` returns `LoggedIn { nest_url, handle }` —
// per-app glue persists `(nest_url, handle, secret)` to its
// long-term identity store.
type ClaimCodeStateClaimed struct {
}

func (e ClaimCodeStateClaimed) Destroy() {
}

// 4xx response (bad code, already-claimed, signature mismatch).
// `submit_enabled` returns to true so the user can retry with a
// corrected code.
type ClaimCodeStateInvalid struct {
	Reason string
}

func (e ClaimCodeStateInvalid) Destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

// 5xx / network error. `transient` mirrors the
// `InviteRequestState::Error` shape: true means "try again later",
// false means "this won't get better on its own."
type ClaimCodeStateError struct {
	Transient bool
	Cause     string
}

func (e ClaimCodeStateError) Destroy() {
	FfiDestroyerBool{}.Destroy(e.Transient)
	FfiDestroyerString{}.Destroy(e.Cause)
}

type FfiConverterClaimCodeState struct{}

var FfiConverterClaimCodeStateINSTANCE = FfiConverterClaimCodeState{}

func (c FfiConverterClaimCodeState) Lift(rb RustBufferI) ClaimCodeState {
	return LiftFromRustBuffer[ClaimCodeState](c, rb)
}

func (c FfiConverterClaimCodeState) Lower(value ClaimCodeState) C.RustBuffer {
	return LowerIntoRustBuffer[ClaimCodeState](c, value)
}

func (c FfiConverterClaimCodeState) LowerExternal(value ClaimCodeState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ClaimCodeState](c, value))
}
func (FfiConverterClaimCodeState) Read(reader io.Reader) ClaimCodeState {
	id := readInt32(reader)
	switch id {
	case 1:
		return ClaimCodeStateIdle{}
	case 2:
		return ClaimCodeStateSubmitting{}
	case 3:
		return ClaimCodeStateClaimed{}
	case 4:
		return ClaimCodeStateInvalid{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 5:
		return ClaimCodeStateError{
			FfiConverterBoolINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterClaimCodeState.Read()", id))
	}
}

func (FfiConverterClaimCodeState) Write(writer io.Writer, value ClaimCodeState) {
	switch variant_value := value.(type) {
	case ClaimCodeStateIdle:
		writeInt32(writer, 1)
	case ClaimCodeStateSubmitting:
		writeInt32(writer, 2)
	case ClaimCodeStateClaimed:
		writeInt32(writer, 3)
	case ClaimCodeStateInvalid:
		writeInt32(writer, 4)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Reason)
	case ClaimCodeStateError:
		writeInt32(writer, 5)
		FfiConverterBoolINSTANCE.Write(writer, variant_value.Transient)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Cause)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterClaimCodeState.Write", value))
	}
}

type FfiDestroyerClaimCodeState struct{}

func (_ FfiDestroyerClaimCodeState) Destroy(value ClaimCodeState) {
	value.Destroy()
}

// Which credential form a `hosted-auth` field is being driven on — the two
// forms hold separate credential bags (`DnsConfigState::creds` /
// `VpsConfigState::creds`), so the sign-in names which one its token lands in.
type CredentialForm uint

const (
	CredentialFormDns CredentialForm = 1
	CredentialFormVps CredentialForm = 2
)

type FfiConverterCredentialForm struct{}

var FfiConverterCredentialFormINSTANCE = FfiConverterCredentialForm{}

func (c FfiConverterCredentialForm) Lift(rb RustBufferI) CredentialForm {
	return LiftFromRustBuffer[CredentialForm](c, rb)
}

func (c FfiConverterCredentialForm) Lower(value CredentialForm) C.RustBuffer {
	return LowerIntoRustBuffer[CredentialForm](c, value)
}

func (c FfiConverterCredentialForm) LowerExternal(value CredentialForm) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[CredentialForm](c, value))
}
func (FfiConverterCredentialForm) Read(reader io.Reader) CredentialForm {
	id := readInt32(reader)
	return CredentialForm(id)
}

func (FfiConverterCredentialForm) Write(writer io.Writer, value CredentialForm) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerCredentialForm struct{}

func (_ FfiDestroyerCredentialForm) Destroy(value CredentialForm) {
}

type ErrorContext uint

const (
	ErrorContextSubmitting ErrorContext = 1
	ErrorContextRechecking ErrorContext = 2
	ErrorContextRedeeming  ErrorContext = 3
)

type FfiConverterErrorContext struct{}

var FfiConverterErrorContextINSTANCE = FfiConverterErrorContext{}

func (c FfiConverterErrorContext) Lift(rb RustBufferI) ErrorContext {
	return LiftFromRustBuffer[ErrorContext](c, rb)
}

func (c FfiConverterErrorContext) Lower(value ErrorContext) C.RustBuffer {
	return LowerIntoRustBuffer[ErrorContext](c, value)
}

func (c FfiConverterErrorContext) LowerExternal(value ErrorContext) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ErrorContext](c, value))
}
func (FfiConverterErrorContext) Read(reader io.Reader) ErrorContext {
	id := readInt32(reader)
	return ErrorContext(id)
}

func (FfiConverterErrorContext) Write(writer io.Writer, value ErrorContext) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerErrorContext struct{}

func (_ FfiDestroyerErrorContext) Destroy(value ErrorContext) {
}

type FieldTypePlain uint

const (
	FieldTypePlainText   FieldTypePlain = 1
	FieldTypePlainSecret FieldTypePlain = 2
	FieldTypePlainSelect FieldTypePlain = 3
	// A hosted sign-in button, not an input: the value is the Bearer token
	// the provider's device-authorization flow yields
	// (`OnboardingMachine::hosted_auth_begin` / `hosted_auth_wait`;
	// `docs/goal/behavior/onboarding.md` § 4). An app that has not built
	// the button renders it as `Secret` — a pasted token is equally valid.
	FieldTypePlainHostedAuth FieldTypePlain = 4
)

type FfiConverterFieldTypePlain struct{}

var FfiConverterFieldTypePlainINSTANCE = FfiConverterFieldTypePlain{}

func (c FfiConverterFieldTypePlain) Lift(rb RustBufferI) FieldTypePlain {
	return LiftFromRustBuffer[FieldTypePlain](c, rb)
}

func (c FfiConverterFieldTypePlain) Lower(value FieldTypePlain) C.RustBuffer {
	return LowerIntoRustBuffer[FieldTypePlain](c, value)
}

func (c FfiConverterFieldTypePlain) LowerExternal(value FieldTypePlain) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[FieldTypePlain](c, value))
}
func (FfiConverterFieldTypePlain) Read(reader io.Reader) FieldTypePlain {
	id := readInt32(reader)
	return FieldTypePlain(id)
}

func (FfiConverterFieldTypePlain) Write(writer io.Writer, value FieldTypePlain) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerFieldTypePlain struct{}

func (_ FfiDestroyerFieldTypePlain) Destroy(value FieldTypePlain) {
}

type HandleCheckOutcome interface {
	Destroy()
}
type HandleCheckOutcomeNone struct {
}

func (e HandleCheckOutcomeNone) Destroy() {
}

type HandleCheckOutcomeFormatInvalid struct {
}

func (e HandleCheckOutcomeFormatInvalid) Destroy() {
}

type HandleCheckOutcomeTldInvalid struct {
}

func (e HandleCheckOutcomeTldInvalid) Destroy() {
}

type HandleCheckOutcomeDomainAvailable struct {
	BuyableViaProvider bool
	Price              *fauna_provisioning.TldPriceQuote
}

func (e HandleCheckOutcomeDomainAvailable) Destroy() {
	FfiDestroyerBool{}.Destroy(e.BuyableViaProvider)
	FfiDestroyerOptionalTldPriceQuote{}.Destroy(e.Price)
}

type HandleCheckOutcomeRegisteredNoNest struct {
}

func (e HandleCheckOutcomeRegisteredNoNest) Destroy() {
}

type HandleCheckOutcomeAlreadyOnNest struct {
	HandleMatches bool
	CurrentHandle *string
}

func (e HandleCheckOutcomeAlreadyOnNest) Destroy() {
	FfiDestroyerBool{}.Destroy(e.HandleMatches)
	FfiDestroyerOptionalString{}.Destroy(e.CurrentHandle)
}

type HandleCheckOutcomeNestRunningUserUnregistered struct {
}

func (e HandleCheckOutcomeNestRunningUserUnregistered) Destroy() {
}

// A nest is reachable on this domain, but `GET /api/v1/setup-status`
// reports `claimed: false` — no admin has claimed it yet. The user's
// only path forward is the claim_code page (Continue routes there
// instead of invite_request, since there is no admin to issue
// invites). Discriminator is binary; no associated data.
type HandleCheckOutcomeUnregisteredUnclaimedNest struct {
}

func (e HandleCheckOutcomeUnregisteredUnclaimedNest) Destroy() {
}

type HandleCheckOutcomeProbeError struct {
	Phase     HandleCheckPhase
	Transient bool
	Cause     string
}

func (e HandleCheckOutcomeProbeError) Destroy() {
	FfiDestroyerHandleCheckPhase{}.Destroy(e.Phase)
	FfiDestroyerBool{}.Destroy(e.Transient)
	FfiDestroyerString{}.Destroy(e.Cause)
}

type FfiConverterHandleCheckOutcome struct{}

var FfiConverterHandleCheckOutcomeINSTANCE = FfiConverterHandleCheckOutcome{}

func (c FfiConverterHandleCheckOutcome) Lift(rb RustBufferI) HandleCheckOutcome {
	return LiftFromRustBuffer[HandleCheckOutcome](c, rb)
}

func (c FfiConverterHandleCheckOutcome) Lower(value HandleCheckOutcome) C.RustBuffer {
	return LowerIntoRustBuffer[HandleCheckOutcome](c, value)
}

func (c FfiConverterHandleCheckOutcome) LowerExternal(value HandleCheckOutcome) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[HandleCheckOutcome](c, value))
}
func (FfiConverterHandleCheckOutcome) Read(reader io.Reader) HandleCheckOutcome {
	id := readInt32(reader)
	switch id {
	case 1:
		return HandleCheckOutcomeNone{}
	case 2:
		return HandleCheckOutcomeFormatInvalid{}
	case 3:
		return HandleCheckOutcomeTldInvalid{}
	case 4:
		return HandleCheckOutcomeDomainAvailable{
			FfiConverterBoolINSTANCE.Read(reader),
			FfiConverterOptionalTldPriceQuoteINSTANCE.Read(reader),
		}
	case 5:
		return HandleCheckOutcomeRegisteredNoNest{}
	case 6:
		return HandleCheckOutcomeAlreadyOnNest{
			FfiConverterBoolINSTANCE.Read(reader),
			FfiConverterOptionalStringINSTANCE.Read(reader),
		}
	case 7:
		return HandleCheckOutcomeNestRunningUserUnregistered{}
	case 8:
		return HandleCheckOutcomeUnregisteredUnclaimedNest{}
	case 9:
		return HandleCheckOutcomeProbeError{
			FfiConverterHandleCheckPhaseINSTANCE.Read(reader),
			FfiConverterBoolINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterHandleCheckOutcome.Read()", id))
	}
}

func (FfiConverterHandleCheckOutcome) Write(writer io.Writer, value HandleCheckOutcome) {
	switch variant_value := value.(type) {
	case HandleCheckOutcomeNone:
		writeInt32(writer, 1)
	case HandleCheckOutcomeFormatInvalid:
		writeInt32(writer, 2)
	case HandleCheckOutcomeTldInvalid:
		writeInt32(writer, 3)
	case HandleCheckOutcomeDomainAvailable:
		writeInt32(writer, 4)
		FfiConverterBoolINSTANCE.Write(writer, variant_value.BuyableViaProvider)
		FfiConverterOptionalTldPriceQuoteINSTANCE.Write(writer, variant_value.Price)
	case HandleCheckOutcomeRegisteredNoNest:
		writeInt32(writer, 5)
	case HandleCheckOutcomeAlreadyOnNest:
		writeInt32(writer, 6)
		FfiConverterBoolINSTANCE.Write(writer, variant_value.HandleMatches)
		FfiConverterOptionalStringINSTANCE.Write(writer, variant_value.CurrentHandle)
	case HandleCheckOutcomeNestRunningUserUnregistered:
		writeInt32(writer, 7)
	case HandleCheckOutcomeUnregisteredUnclaimedNest:
		writeInt32(writer, 8)
	case HandleCheckOutcomeProbeError:
		writeInt32(writer, 9)
		FfiConverterHandleCheckPhaseINSTANCE.Write(writer, variant_value.Phase)
		FfiConverterBoolINSTANCE.Write(writer, variant_value.Transient)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Cause)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterHandleCheckOutcome.Write", value))
	}
}

type FfiDestroyerHandleCheckOutcome struct{}

func (_ FfiDestroyerHandleCheckOutcome) Destroy(value HandleCheckOutcome) {
	value.Destroy()
}

type HandleCheckPhase uint

const (
	HandleCheckPhaseIdle              HandleCheckPhase = 1
	HandleCheckPhaseParsing           HandleCheckPhase = 2
	HandleCheckPhaseDnsLookup         HandleCheckPhase = 3
	HandleCheckPhaseNestProbe         HandleCheckPhase = 4
	HandleCheckPhaseChallengeResponse HandleCheckPhase = 5
	HandleCheckPhasePriceLookup       HandleCheckPhase = 6
	HandleCheckPhaseComplete          HandleCheckPhase = 7
)

type FfiConverterHandleCheckPhase struct{}

var FfiConverterHandleCheckPhaseINSTANCE = FfiConverterHandleCheckPhase{}

func (c FfiConverterHandleCheckPhase) Lift(rb RustBufferI) HandleCheckPhase {
	return LiftFromRustBuffer[HandleCheckPhase](c, rb)
}

func (c FfiConverterHandleCheckPhase) Lower(value HandleCheckPhase) C.RustBuffer {
	return LowerIntoRustBuffer[HandleCheckPhase](c, value)
}

func (c FfiConverterHandleCheckPhase) LowerExternal(value HandleCheckPhase) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[HandleCheckPhase](c, value))
}
func (FfiConverterHandleCheckPhase) Read(reader io.Reader) HandleCheckPhase {
	id := readInt32(reader)
	return HandleCheckPhase(id)
}

func (FfiConverterHandleCheckPhase) Write(writer io.Writer, value HandleCheckPhase) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerHandleCheckPhase struct{}

func (_ FfiDestroyerHandleCheckPhase) Destroy(value HandleCheckPhase) {
}

// Where a `hosted-auth` field's sign-in stands. Drives the field's button
// label on every app (`docs/goal/behavior/onboarding.md` § 4):
// `Idle` → "Sign in at the provider…", `Pending` → "Finish signing in in your
// browser — code {user_code}", `Connected`, `Failed`.
type HostedAuthState interface {
	Destroy()
}
type HostedAuthStateIdle struct {
}

func (e HostedAuthStateIdle) Destroy() {
}

// The device-authorization request is out; the user is (or should be)
// on the provider's hosted page. `verification_url` is what the app
// opened, `user_code` the fallback the user can type there.
type HostedAuthStatePending struct {
	UserCode        string
	VerificationUrl string
}

func (e HostedAuthStatePending) Destroy() {
	FfiDestroyerString{}.Destroy(e.UserCode)
	FfiDestroyerString{}.Destroy(e.VerificationUrl)
}

// The token landed in the form's credential bag under the field id.
type HostedAuthStateConnected struct {
}

func (e HostedAuthStateConnected) Destroy() {
}

// The attempt ended without a token (declined, expired, or a transport
// error) — the button offers to start over.
type HostedAuthStateFailed struct {
	Message string
}

func (e HostedAuthStateFailed) Destroy() {
	FfiDestroyerString{}.Destroy(e.Message)
}

type FfiConverterHostedAuthState struct{}

var FfiConverterHostedAuthStateINSTANCE = FfiConverterHostedAuthState{}

func (c FfiConverterHostedAuthState) Lift(rb RustBufferI) HostedAuthState {
	return LiftFromRustBuffer[HostedAuthState](c, rb)
}

func (c FfiConverterHostedAuthState) Lower(value HostedAuthState) C.RustBuffer {
	return LowerIntoRustBuffer[HostedAuthState](c, value)
}

func (c FfiConverterHostedAuthState) LowerExternal(value HostedAuthState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[HostedAuthState](c, value))
}
func (FfiConverterHostedAuthState) Read(reader io.Reader) HostedAuthState {
	id := readInt32(reader)
	switch id {
	case 1:
		return HostedAuthStateIdle{}
	case 2:
		return HostedAuthStatePending{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 3:
		return HostedAuthStateConnected{}
	case 4:
		return HostedAuthStateFailed{
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterHostedAuthState.Read()", id))
	}
}

func (FfiConverterHostedAuthState) Write(writer io.Writer, value HostedAuthState) {
	switch variant_value := value.(type) {
	case HostedAuthStateIdle:
		writeInt32(writer, 1)
	case HostedAuthStatePending:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variant_value.UserCode)
		FfiConverterStringINSTANCE.Write(writer, variant_value.VerificationUrl)
	case HostedAuthStateConnected:
		writeInt32(writer, 3)
	case HostedAuthStateFailed:
		writeInt32(writer, 4)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Message)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterHostedAuthState.Write", value))
	}
}

type FfiDestroyerHostedAuthState struct{}

func (_ FfiDestroyerHostedAuthState) Destroy(value HostedAuthState) {
	value.Destroy()
}

type IdentityOrigin uint

const (
	IdentityOriginCreated  IdentityOrigin = 1
	IdentityOriginImported IdentityOrigin = 2
)

type FfiConverterIdentityOrigin struct{}

var FfiConverterIdentityOriginINSTANCE = FfiConverterIdentityOrigin{}

func (c FfiConverterIdentityOrigin) Lift(rb RustBufferI) IdentityOrigin {
	return LiftFromRustBuffer[IdentityOrigin](c, rb)
}

func (c FfiConverterIdentityOrigin) Lower(value IdentityOrigin) C.RustBuffer {
	return LowerIntoRustBuffer[IdentityOrigin](c, value)
}

func (c FfiConverterIdentityOrigin) LowerExternal(value IdentityOrigin) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[IdentityOrigin](c, value))
}
func (FfiConverterIdentityOrigin) Read(reader io.Reader) IdentityOrigin {
	id := readInt32(reader)
	return IdentityOrigin(id)
}

func (FfiConverterIdentityOrigin) Write(writer io.Writer, value IdentityOrigin) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerIdentityOrigin struct{}

func (_ FfiDestroyerIdentityOrigin) Destroy(value IdentityOrigin) {
}

type InviteRequestState interface {
	Destroy()
}
type InviteRequestStateIdle struct {
}

func (e InviteRequestStateIdle) Destroy() {
}

type InviteRequestStateSubmitting struct {
}

func (e InviteRequestStateSubmitting) Destroy() {
}

type InviteRequestStateRechecking struct {
}

func (e InviteRequestStateRechecking) Destroy() {
}

type InviteRequestStateDenied struct {
	Reason    string
	RequestId string
}

func (e InviteRequestStateDenied) Destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
	FfiDestroyerString{}.Destroy(e.RequestId)
}

type InviteRequestStatePendingReview struct {
	RequestId     string
	LastCheckedMs uint64
}

func (e InviteRequestStatePendingReview) Destroy() {
	FfiDestroyerString{}.Destroy(e.RequestId)
	FfiDestroyerUint64{}.Destroy(e.LastCheckedMs)
}

type InviteRequestStateError struct {
	Transient bool
	Context   ErrorContext
	Cause     string
}

func (e InviteRequestStateError) Destroy() {
	FfiDestroyerBool{}.Destroy(e.Transient)
	FfiDestroyerErrorContext{}.Destroy(e.Context)
	FfiDestroyerString{}.Destroy(e.Cause)
}

type FfiConverterInviteRequestState struct{}

var FfiConverterInviteRequestStateINSTANCE = FfiConverterInviteRequestState{}

func (c FfiConverterInviteRequestState) Lift(rb RustBufferI) InviteRequestState {
	return LiftFromRustBuffer[InviteRequestState](c, rb)
}

func (c FfiConverterInviteRequestState) Lower(value InviteRequestState) C.RustBuffer {
	return LowerIntoRustBuffer[InviteRequestState](c, value)
}

func (c FfiConverterInviteRequestState) LowerExternal(value InviteRequestState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[InviteRequestState](c, value))
}
func (FfiConverterInviteRequestState) Read(reader io.Reader) InviteRequestState {
	id := readInt32(reader)
	switch id {
	case 1:
		return InviteRequestStateIdle{}
	case 2:
		return InviteRequestStateSubmitting{}
	case 3:
		return InviteRequestStateRechecking{}
	case 4:
		return InviteRequestStateDenied{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 5:
		return InviteRequestStatePendingReview{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterUint64INSTANCE.Read(reader),
		}
	case 6:
		return InviteRequestStateError{
			FfiConverterBoolINSTANCE.Read(reader),
			FfiConverterErrorContextINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterInviteRequestState.Read()", id))
	}
}

func (FfiConverterInviteRequestState) Write(writer io.Writer, value InviteRequestState) {
	switch variant_value := value.(type) {
	case InviteRequestStateIdle:
		writeInt32(writer, 1)
	case InviteRequestStateSubmitting:
		writeInt32(writer, 2)
	case InviteRequestStateRechecking:
		writeInt32(writer, 3)
	case InviteRequestStateDenied:
		writeInt32(writer, 4)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Reason)
		FfiConverterStringINSTANCE.Write(writer, variant_value.RequestId)
	case InviteRequestStatePendingReview:
		writeInt32(writer, 5)
		FfiConverterStringINSTANCE.Write(writer, variant_value.RequestId)
		FfiConverterUint64INSTANCE.Write(writer, variant_value.LastCheckedMs)
	case InviteRequestStateError:
		writeInt32(writer, 6)
		FfiConverterBoolINSTANCE.Write(writer, variant_value.Transient)
		FfiConverterErrorContextINSTANCE.Write(writer, variant_value.Context)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Cause)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterInviteRequestState.Write", value))
	}
}

type FfiDestroyerInviteRequestState struct{}

func (_ FfiDestroyerInviteRequestState) Destroy(value InviteRequestState) {
	value.Destroy()
}

// Why a by-hand record is on the list — what the view keys its text on, so
// the urgent records read as urgent rather than as four more stale `TXT`.
type LeftoverKind uint

const (
	// Shares a name with non-Fauna records and carries no pointer to the box:
	// harmless stale, remove at leisure.
	LeftoverKindSharedName LeftoverKind = 1
	// Still points at the box being destroyed, and no credential could remove
	// it: remove this one *before* the address is released.
	LeftoverKindPointsAtBox LeftoverKind = 2
)

type FfiConverterLeftoverKind struct{}

var FfiConverterLeftoverKindINSTANCE = FfiConverterLeftoverKind{}

func (c FfiConverterLeftoverKind) Lift(rb RustBufferI) LeftoverKind {
	return LiftFromRustBuffer[LeftoverKind](c, rb)
}

func (c FfiConverterLeftoverKind) Lower(value LeftoverKind) C.RustBuffer {
	return LowerIntoRustBuffer[LeftoverKind](c, value)
}

func (c FfiConverterLeftoverKind) LowerExternal(value LeftoverKind) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[LeftoverKind](c, value))
}
func (FfiConverterLeftoverKind) Read(reader io.Reader) LeftoverKind {
	id := readInt32(reader)
	return LeftoverKind(id)
}

func (FfiConverterLeftoverKind) Write(writer io.Writer, value LeftoverKind) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerLeftoverKind struct{}

func (_ FfiDestroyerLeftoverKind) Destroy(value LeftoverKind) {
}

type NatModeState interface {
	Destroy()
}

// Initial / picking. Submit enabled. The pre-selection is the nest's
// resolved seed with the one-directional private-ward refinement
// (`reset_nat_mode_snapshot`), so the common case is confirm-only.
type NatModeStateChoosing struct {
}

func (e NatModeStateChoosing) Destroy() {
}

// `submit_nat_mode_choice` is in flight. Submit disabled.
type NatModeStateSubmitting struct {
}

func (e NatModeStateSubmitting) Destroy() {
}

// Server upserted the row. The wizard transitions to `Done` with
// `wizard_outcome() == LoggedIn`.
type NatModeStateDone struct {
}

func (e NatModeStateDone) Destroy() {
}

// Submit failed. `transient: true` means transport/internal — retry may
// help. `transient: false` means a 4xx-class reject (`not_claimed`,
// `signature_failed`, `invalid_request`). Submit stays enabled either
// way — the set is mutable, resubmit is always allowed.
type NatModeStateError struct {
	Transient bool
	Cause     string
}

func (e NatModeStateError) Destroy() {
	FfiDestroyerBool{}.Destroy(e.Transient)
	FfiDestroyerString{}.Destroy(e.Cause)
}

type FfiConverterNatModeState struct{}

var FfiConverterNatModeStateINSTANCE = FfiConverterNatModeState{}

func (c FfiConverterNatModeState) Lift(rb RustBufferI) NatModeState {
	return LiftFromRustBuffer[NatModeState](c, rb)
}

func (c FfiConverterNatModeState) Lower(value NatModeState) C.RustBuffer {
	return LowerIntoRustBuffer[NatModeState](c, value)
}

func (c FfiConverterNatModeState) LowerExternal(value NatModeState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[NatModeState](c, value))
}
func (FfiConverterNatModeState) Read(reader io.Reader) NatModeState {
	id := readInt32(reader)
	switch id {
	case 1:
		return NatModeStateChoosing{}
	case 2:
		return NatModeStateSubmitting{}
	case 3:
		return NatModeStateDone{}
	case 4:
		return NatModeStateError{
			FfiConverterBoolINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterNatModeState.Read()", id))
	}
}

func (FfiConverterNatModeState) Write(writer io.Writer, value NatModeState) {
	switch variant_value := value.(type) {
	case NatModeStateChoosing:
		writeInt32(writer, 1)
	case NatModeStateSubmitting:
		writeInt32(writer, 2)
	case NatModeStateDone:
		writeInt32(writer, 3)
	case NatModeStateError:
		writeInt32(writer, 4)
		FfiConverterBoolINSTANCE.Write(writer, variant_value.Transient)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Cause)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterNatModeState.Write", value))
	}
}

type FfiDestroyerNatModeState struct{}

func (_ FfiDestroyerNatModeState) Destroy(value NatModeState) {
	value.Destroy()
}

type OnboardingError struct {
	err error
}

// Convenience method to turn *OnboardingError into error
// Avoiding treating nil pointer as non nil error interface
func (err *OnboardingError) AsError() error {
	if err == nil {
		return nil
	} else {
		return err
	}
}

func (err OnboardingError) Error() string {
	return fmt.Sprintf("OnboardingError: %s", err.err.Error())
}

func (err OnboardingError) Unwrap() error {
	return err.err
}

// Err* are used for checking error type with `errors.Is`
var ErrOnboardingErrorNetwork = fmt.Errorf("OnboardingErrorNetwork")
var ErrOnboardingErrorDomainProbeFailed = fmt.Errorf("OnboardingErrorDomainProbeFailed")
var ErrOnboardingErrorDomainNotAvailable = fmt.Errorf("OnboardingErrorDomainNotAvailable")
var ErrOnboardingErrorProviderUnauthorized = fmt.Errorf("OnboardingErrorProviderUnauthorized")
var ErrOnboardingErrorProviderProtocolError = fmt.Errorf("OnboardingErrorProviderProtocolError")
var ErrOnboardingErrorProvisioningFailed = fmt.Errorf("OnboardingErrorProvisioningFailed")
var ErrOnboardingErrorClaimFailed = fmt.Errorf("OnboardingErrorClaimFailed")
var ErrOnboardingErrorLoginFailed = fmt.Errorf("OnboardingErrorLoginFailed")
var ErrOnboardingErrorInvalidTransition = fmt.Errorf("OnboardingErrorInvalidTransition")
var ErrOnboardingErrorOther = fmt.Errorf("OnboardingErrorOther")

// Variant structs
// Network / HTTP error — generic transport failure.
//
// Field is named `detail` (not `message`) so the UniFFI Kotlin
// generator doesn't collide with `Throwable.message` on the
// generated `OnboardingException.Network` class.
type OnboardingErrorNetwork struct {
	Detail string
}

// Network / HTTP error — generic transport failure.
//
// Field is named `detail` (not `message`) so the UniFFI Kotlin
// generator doesn't collide with `Throwable.message` on the
// generated `OnboardingException.Network` class.
func NewOnboardingErrorNetwork(
	detail string,
) *OnboardingError {
	return &OnboardingError{err: &OnboardingErrorNetwork{
		Detail: detail}}
}

func (e OnboardingErrorNetwork) destroy() {
	FfiDestroyerString{}.Destroy(e.Detail)
}

func (err OnboardingErrorNetwork) Error() string {
	return fmt.Sprint("Network",
		": ",

		"Detail=",
		err.Detail,
	)
}

func (self OnboardingErrorNetwork) Is(target error) bool {
	return target == ErrOnboardingErrorNetwork
}

// Domain was unregistered or otherwise rejected by the probe.
type OnboardingErrorDomainProbeFailed struct {
	Domain string
	Reason string
}

// Domain was unregistered or otherwise rejected by the probe.
func NewOnboardingErrorDomainProbeFailed(
	domain string,
	reason string,
) *OnboardingError {
	return &OnboardingError{err: &OnboardingErrorDomainProbeFailed{
		Domain: domain,
		Reason: reason}}
}

func (e OnboardingErrorDomainProbeFailed) destroy() {
	FfiDestroyerString{}.Destroy(e.Domain)
	FfiDestroyerString{}.Destroy(e.Reason)
}

func (err OnboardingErrorDomainProbeFailed) Error() string {
	return fmt.Sprint("DomainProbeFailed",
		": ",

		"Domain=",
		err.Domain,
		", ",
		"Reason=",
		err.Reason,
	)
}

func (self OnboardingErrorDomainProbeFailed) Is(target error) bool {
	return target == ErrOnboardingErrorDomainProbeFailed
}

// Tried to register a domain that's already taken.
type OnboardingErrorDomainNotAvailable struct {
	Domain string
}

// Tried to register a domain that's already taken.
func NewOnboardingErrorDomainNotAvailable(
	domain string,
) *OnboardingError {
	return &OnboardingError{err: &OnboardingErrorDomainNotAvailable{
		Domain: domain}}
}

func (e OnboardingErrorDomainNotAvailable) destroy() {
	FfiDestroyerString{}.Destroy(e.Domain)
}

func (err OnboardingErrorDomainNotAvailable) Error() string {
	return fmt.Sprint("DomainNotAvailable",
		": ",

		"Domain=",
		err.Domain,
	)
}

func (self OnboardingErrorDomainNotAvailable) Is(target error) bool {
	return target == ErrOnboardingErrorDomainNotAvailable
}

// Provider rejected the credentials.
type OnboardingErrorProviderUnauthorized struct {
	Provider string
}

// Provider rejected the credentials.
func NewOnboardingErrorProviderUnauthorized(
	provider string,
) *OnboardingError {
	return &OnboardingError{err: &OnboardingErrorProviderUnauthorized{
		Provider: provider}}
}

func (e OnboardingErrorProviderUnauthorized) destroy() {
	FfiDestroyerString{}.Destroy(e.Provider)
}

func (err OnboardingErrorProviderUnauthorized) Error() string {
	return fmt.Sprint("ProviderUnauthorized",
		": ",

		"Provider=",
		err.Provider,
	)
}

func (self OnboardingErrorProviderUnauthorized) Is(target error) bool {
	return target == ErrOnboardingErrorProviderUnauthorized
}

// Provider returned a malformed response.
type OnboardingErrorProviderProtocolError struct {
	Provider string
	Detail   string
}

// Provider returned a malformed response.
func NewOnboardingErrorProviderProtocolError(
	provider string,
	detail string,
) *OnboardingError {
	return &OnboardingError{err: &OnboardingErrorProviderProtocolError{
		Provider: provider,
		Detail:   detail}}
}

func (e OnboardingErrorProviderProtocolError) destroy() {
	FfiDestroyerString{}.Destroy(e.Provider)
	FfiDestroyerString{}.Destroy(e.Detail)
}

func (err OnboardingErrorProviderProtocolError) Error() string {
	return fmt.Sprint("ProviderProtocolError",
		": ",

		"Provider=",
		err.Provider,
		", ",
		"Detail=",
		err.Detail,
	)
}

func (self OnboardingErrorProviderProtocolError) Is(target error) bool {
	return target == ErrOnboardingErrorProviderProtocolError
}

// Provisioning step failed mid-flight (server creation, DNS write, …).
type OnboardingErrorProvisioningFailed struct {
	Step   string
	Detail string
}

// Provisioning step failed mid-flight (server creation, DNS write, …).
func NewOnboardingErrorProvisioningFailed(
	step string,
	detail string,
) *OnboardingError {
	return &OnboardingError{err: &OnboardingErrorProvisioningFailed{
		Step:   step,
		Detail: detail}}
}

func (e OnboardingErrorProvisioningFailed) destroy() {
	FfiDestroyerString{}.Destroy(e.Step)
	FfiDestroyerString{}.Destroy(e.Detail)
}

func (err OnboardingErrorProvisioningFailed) Error() string {
	return fmt.Sprint("ProvisioningFailed",
		": ",

		"Step=",
		err.Step,
		", ",
		"Detail=",
		err.Detail,
	)
}

func (self OnboardingErrorProvisioningFailed) Is(target error) bool {
	return target == ErrOnboardingErrorProvisioningFailed
}

// Nest claim failed (wrong code, expired, etc.).
type OnboardingErrorClaimFailed struct {
	Reason string
}

// Nest claim failed (wrong code, expired, etc.).
func NewOnboardingErrorClaimFailed(
	reason string,
) *OnboardingError {
	return &OnboardingError{err: &OnboardingErrorClaimFailed{
		Reason: reason}}
}

func (e OnboardingErrorClaimFailed) destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

func (err OnboardingErrorClaimFailed) Error() string {
	return fmt.Sprint("ClaimFailed",
		": ",

		"Reason=",
		err.Reason,
	)
}

func (self OnboardingErrorClaimFailed) Is(target error) bool {
	return target == ErrOnboardingErrorClaimFailed
}

// Silent challenge or login failed.
type OnboardingErrorLoginFailed struct {
	Reason string
}

// Silent challenge or login failed.
func NewOnboardingErrorLoginFailed(
	reason string,
) *OnboardingError {
	return &OnboardingError{err: &OnboardingErrorLoginFailed{
		Reason: reason}}
}

func (e OnboardingErrorLoginFailed) destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

func (err OnboardingErrorLoginFailed) Error() string {
	return fmt.Sprint("LoginFailed",
		": ",

		"Reason=",
		err.Reason,
	)
}

func (self OnboardingErrorLoginFailed) Is(target error) bool {
	return target == ErrOnboardingErrorLoginFailed
}

// Caller invoked a transition that the current state doesn't allow
// (e.g. continue_from_dns when no provider is selected).
type OnboardingErrorInvalidTransition struct {
	From   string
	Reason string
}

// Caller invoked a transition that the current state doesn't allow
// (e.g. continue_from_dns when no provider is selected).
func NewOnboardingErrorInvalidTransition(
	from string,
	reason string,
) *OnboardingError {
	return &OnboardingError{err: &OnboardingErrorInvalidTransition{
		From:   from,
		Reason: reason}}
}

func (e OnboardingErrorInvalidTransition) destroy() {
	FfiDestroyerString{}.Destroy(e.From)
	FfiDestroyerString{}.Destroy(e.Reason)
}

func (err OnboardingErrorInvalidTransition) Error() string {
	return fmt.Sprint("InvalidTransition",
		": ",

		"From=",
		err.From,
		", ",
		"Reason=",
		err.Reason,
	)
}

func (self OnboardingErrorInvalidTransition) Is(target error) bool {
	return target == ErrOnboardingErrorInvalidTransition
}

// Generic fallback for sources without a specific category.
//
// Field is `detail` for the same Kotlin-collision reason as `Network`.
type OnboardingErrorOther struct {
	Detail string
}

// Generic fallback for sources without a specific category.
//
// Field is `detail` for the same Kotlin-collision reason as `Network`.
func NewOnboardingErrorOther(
	detail string,
) *OnboardingError {
	return &OnboardingError{err: &OnboardingErrorOther{
		Detail: detail}}
}

func (e OnboardingErrorOther) destroy() {
	FfiDestroyerString{}.Destroy(e.Detail)
}

func (err OnboardingErrorOther) Error() string {
	return fmt.Sprint("Other",
		": ",

		"Detail=",
		err.Detail,
	)
}

func (self OnboardingErrorOther) Is(target error) bool {
	return target == ErrOnboardingErrorOther
}

type FfiConverterOnboardingError struct{}

var FfiConverterOnboardingErrorINSTANCE = FfiConverterOnboardingError{}

func (c FfiConverterOnboardingError) Lift(eb RustBufferI) *OnboardingError {
	return LiftFromRustBuffer[*OnboardingError](c, eb)
}

func (c FfiConverterOnboardingError) Lower(value *OnboardingError) C.RustBuffer {
	return LowerIntoRustBuffer[*OnboardingError](c, value)
}

func (c FfiConverterOnboardingError) LowerExternal(value *OnboardingError) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*OnboardingError](c, value))
}

func (c FfiConverterOnboardingError) Read(reader io.Reader) *OnboardingError {
	errorID := readUint32(reader)

	switch errorID {
	case 1:
		return &OnboardingError{&OnboardingErrorNetwork{
			Detail: FfiConverterStringINSTANCE.Read(reader),
		}}
	case 2:
		return &OnboardingError{&OnboardingErrorDomainProbeFailed{
			Domain: FfiConverterStringINSTANCE.Read(reader),
			Reason: FfiConverterStringINSTANCE.Read(reader),
		}}
	case 3:
		return &OnboardingError{&OnboardingErrorDomainNotAvailable{
			Domain: FfiConverterStringINSTANCE.Read(reader),
		}}
	case 4:
		return &OnboardingError{&OnboardingErrorProviderUnauthorized{
			Provider: FfiConverterStringINSTANCE.Read(reader),
		}}
	case 5:
		return &OnboardingError{&OnboardingErrorProviderProtocolError{
			Provider: FfiConverterStringINSTANCE.Read(reader),
			Detail:   FfiConverterStringINSTANCE.Read(reader),
		}}
	case 6:
		return &OnboardingError{&OnboardingErrorProvisioningFailed{
			Step:   FfiConverterStringINSTANCE.Read(reader),
			Detail: FfiConverterStringINSTANCE.Read(reader),
		}}
	case 7:
		return &OnboardingError{&OnboardingErrorClaimFailed{
			Reason: FfiConverterStringINSTANCE.Read(reader),
		}}
	case 8:
		return &OnboardingError{&OnboardingErrorLoginFailed{
			Reason: FfiConverterStringINSTANCE.Read(reader),
		}}
	case 9:
		return &OnboardingError{&OnboardingErrorInvalidTransition{
			From:   FfiConverterStringINSTANCE.Read(reader),
			Reason: FfiConverterStringINSTANCE.Read(reader),
		}}
	case 10:
		return &OnboardingError{&OnboardingErrorOther{
			Detail: FfiConverterStringINSTANCE.Read(reader),
		}}
	default:
		panic(fmt.Sprintf("Unknown error code %d in FfiConverterOnboardingError.Read()", errorID))
	}
}

func (c FfiConverterOnboardingError) Write(writer io.Writer, value *OnboardingError) {
	switch variantValue := value.err.(type) {
	case *OnboardingErrorNetwork:
		writeInt32(writer, 1)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Detail)
	case *OnboardingErrorDomainProbeFailed:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Domain)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Reason)
	case *OnboardingErrorDomainNotAvailable:
		writeInt32(writer, 3)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Domain)
	case *OnboardingErrorProviderUnauthorized:
		writeInt32(writer, 4)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Provider)
	case *OnboardingErrorProviderProtocolError:
		writeInt32(writer, 5)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Provider)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Detail)
	case *OnboardingErrorProvisioningFailed:
		writeInt32(writer, 6)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Step)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Detail)
	case *OnboardingErrorClaimFailed:
		writeInt32(writer, 7)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Reason)
	case *OnboardingErrorLoginFailed:
		writeInt32(writer, 8)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Reason)
	case *OnboardingErrorInvalidTransition:
		writeInt32(writer, 9)
		FfiConverterStringINSTANCE.Write(writer, variantValue.From)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Reason)
	case *OnboardingErrorOther:
		writeInt32(writer, 10)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Detail)
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiConverterOnboardingError.Write", value))
	}
}

type FfiDestroyerOnboardingError struct{}

func (_ FfiDestroyerOnboardingError) Destroy(value *OnboardingError) {
	switch variantValue := value.err.(type) {
	case OnboardingErrorNetwork:
		variantValue.destroy()
	case OnboardingErrorDomainProbeFailed:
		variantValue.destroy()
	case OnboardingErrorDomainNotAvailable:
		variantValue.destroy()
	case OnboardingErrorProviderUnauthorized:
		variantValue.destroy()
	case OnboardingErrorProviderProtocolError:
		variantValue.destroy()
	case OnboardingErrorProvisioningFailed:
		variantValue.destroy()
	case OnboardingErrorClaimFailed:
		variantValue.destroy()
	case OnboardingErrorLoginFailed:
		variantValue.destroy()
	case OnboardingErrorInvalidTransition:
		variantValue.destroy()
	case OnboardingErrorOther:
		variantValue.destroy()
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiDestroyerOnboardingError.Destroy", value))
	}
}

type OnboardingStep uint

const (
	OnboardingStepIdentityChoice  OnboardingStep = 1
	OnboardingStepIdentityCreated OnboardingStep = 2
	OnboardingStepIdentityImport  OnboardingStep = 3
	// The recovery-kit offer, right after the identity secret
	// (`onboarding.md` § 1 Identity; position is a user ruling, 2026-07-23).
	// The screen **mints and displays only** — no nest exists at this point,
	// so registration + escrow run at the wizard's signed-in handoff
	// (`identity-succession.md` § The RecoveryKey → *Creation UX*, ratified
	// 2026-08-01). Entered from `confirm_generated_identity` **only when the
	// app declared `set_renders_recovery_kit(true)`** — apps whose screen
	// hasn't landed keep going straight to `HandleEntry`, a per-app parity
	// gap, not drift. Confirm and skip both land on `HandleEntry`.
	OnboardingStepRecoveryKit OnboardingStep = 4
	// The phrase-only identity restore (`onboarding.md` § 1 Identity),
	// reached from `identity_choice` via `restore-from-recovery-kit-button` —
	// so only apps that render that button ever enter it; no capability flag
	// is needed. On success the identity seed is restored and the wizard
	// lands on `HandleEntry`, uniform with an import. Distinct from
	// [`BoxRecoveryEntry`], which names the total-box-loss NEST recovery.
	OnboardingStepRecoveryEntry       OnboardingStep = 5
	OnboardingStepHandleEntry         OnboardingStep = 6
	OnboardingStepDnsConfig           OnboardingStep = 7
	OnboardingStepVpsConfig           OnboardingStep = 8
	OnboardingStepNestProvisioning    OnboardingStep = 9
	OnboardingStepDnsPostInstructions OnboardingStep = 10
	OnboardingStepInviteRequest       OnboardingStep = 11
	// Reached only when handle-check returns
	// `HandleCheckOutcome::UnregisteredUnclaimedNest` — the nest is
	// running but `setup-status.claimed == false`. Mutually exclusive
	// with `InviteRequest`: there's no admin to issue invites yet, so
	// the user's only path forward is the one-time claim code printed
	// by the nest's bootstrap process. See
	// `docs/goal/behavior/onboarding.md` §3a.
	OnboardingStepClaimCode OnboardingStep = 12
	// The `nat_mode_choice` page — the **single, terminal** admin-path setup
	// step: the admin confirms the nest's NAT axis (`public` / `private`).
	// Reached **directly on claim completion** — there is no storage-mode
	// question, because there is no storage mode (`storage-modes.md`; a nest
	// is sealed and content-ready from first boot).
	// `submit_nat_mode_choice` commits via the mutable `fauna.setup.nat_mode`
	// kind and exits to `Done` with `wizard_outcome() == LoggedIn`;
	// `defer_nat_mode_choice` exits the same way keeping the seeded value. Per
	// `docs/goal/behavior/onboarding.md` § 3b-bis.
	OnboardingStepNatModeChoice OnboardingStep = 13
	// The one-tap "trust this box" offer (`onboarding.md` § 3b-ter; ui.yaml
	// page `onboarding.trust_prompt`) — an **optional interstitial before
	// LoggedIn, not a setup step**: it configures nothing nest-side, so
	// [`Self::NatModeChoice`] stays the terminal admin-path *setup* step.
	//
	// The screen **asks only.** Minting the default grant set needs an
	// authenticated session and the nest's content-processor roster, neither
	// of which the wizard holds, so the answer is latched
	// ([`crate::OnboardingMachine::take_trust_prompt_granted`]) and the mint
	// runs at the signed-in handoff — the same deferral
	// [`Self::RecoveryKit`] uses for registration + escrow. Entered from
	// every route that *establishes* the user on the nest — the NAT step's
	// two exits on the claim path, plus an invite redemption and an approved
	// join request (§ 3b-ter's "joining user's first login") — and **only
	// when the app declared `set_renders_trust_prompt(true)`**; apps whose
	// screen hasn't landed keep going straight to [`Self::Done`], a per-app
	// parity gap, not drift. A returning `AlreadyOnNest` sign-in is NOT such
	// a route and never reaches here. Grant and skip both land on
	// [`Self::Done`] with the same `LoggedIn` outcome.
	OnboardingStepTrustPrompt OnboardingStep = 14
	// Total-box-loss recovery hub (`box-recovery.md` § Recovery UI, step 4;
	// ui.yaml page `nest_recovery`). Lists the admin's custodied boxes
	// (`fauna.state.deployment-seeds`, read via the shared `deploymentSeeds()`
	// getter — this device's store joined with a reachable nest's); the admin selects one and picks a re-provision method (cloud vs
	// self-hosted). Reached from two entries: `launch-recover-button` on
	// `launch_retry` (surviving device) and `recover-lost-box-button` on
	// `identity_choice` → `identity_import` (recovery intent) → here.
	OnboardingStepNestRecovery OnboardingStep = 15
	// Self-hosted recovery instructions (`box-recovery.md` § Recovery UI,
	// step 4; ui.yaml page `recover_selfhosted_instructions`). Shows the
	// installer invocation / `.env` line carrying `FAUNA_DEPLOYMENT_SEED` (the
	// client-custodied seed for the selected box) so `docker compose up` boots
	// the box with the same `nest_actor_id`. An out-of-band step — the box
	// isn't up yet, so continuing exits the wizard.
	OnboardingStepRecoverSelfhostedInstructions OnboardingStep = 16
	// Sentinel: the wizard finished. App router should swap to the
	// authenticated UI. Caller may also query `wizard_outcome()` for the
	// terminal `WizardOutcome` and route to the main app.
	OnboardingStepDone OnboardingStep = 17
)

type FfiConverterOnboardingStep struct{}

var FfiConverterOnboardingStepINSTANCE = FfiConverterOnboardingStep{}

func (c FfiConverterOnboardingStep) Lift(rb RustBufferI) OnboardingStep {
	return LiftFromRustBuffer[OnboardingStep](c, rb)
}

func (c FfiConverterOnboardingStep) Lower(value OnboardingStep) C.RustBuffer {
	return LowerIntoRustBuffer[OnboardingStep](c, value)
}

func (c FfiConverterOnboardingStep) LowerExternal(value OnboardingStep) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[OnboardingStep](c, value))
}
func (FfiConverterOnboardingStep) Read(reader io.Reader) OnboardingStep {
	id := readInt32(reader)
	return OnboardingStep(id)
}

func (FfiConverterOnboardingStep) Write(writer io.Writer, value OnboardingStep) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerOnboardingStep struct{}

func (_ FfiDestroyerOnboardingStep) Destroy(value OnboardingStep) {
}

type OobCodeState interface {
	Destroy()
}
type OobCodeStateIdle struct {
}

func (e OobCodeStateIdle) Destroy() {
}

type OobCodeStateVerifying struct {
}

func (e OobCodeStateVerifying) Destroy() {
}

type OobCodeStateValid struct {
	InviteId     string
	SupervisedBy *string
}

func (e OobCodeStateValid) Destroy() {
	FfiDestroyerString{}.Destroy(e.InviteId)
	FfiDestroyerOptionalString{}.Destroy(e.SupervisedBy)
}

type OobCodeStateInvalid struct {
	Reason string
}

func (e OobCodeStateInvalid) Destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

type OobCodeStateError struct {
	Cause string
}

func (e OobCodeStateError) Destroy() {
	FfiDestroyerString{}.Destroy(e.Cause)
}

type FfiConverterOobCodeState struct{}

var FfiConverterOobCodeStateINSTANCE = FfiConverterOobCodeState{}

func (c FfiConverterOobCodeState) Lift(rb RustBufferI) OobCodeState {
	return LiftFromRustBuffer[OobCodeState](c, rb)
}

func (c FfiConverterOobCodeState) Lower(value OobCodeState) C.RustBuffer {
	return LowerIntoRustBuffer[OobCodeState](c, value)
}

func (c FfiConverterOobCodeState) LowerExternal(value OobCodeState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[OobCodeState](c, value))
}
func (FfiConverterOobCodeState) Read(reader io.Reader) OobCodeState {
	id := readInt32(reader)
	switch id {
	case 1:
		return OobCodeStateIdle{}
	case 2:
		return OobCodeStateVerifying{}
	case 3:
		return OobCodeStateValid{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterOptionalStringINSTANCE.Read(reader),
		}
	case 4:
		return OobCodeStateInvalid{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 5:
		return OobCodeStateError{
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterOobCodeState.Read()", id))
	}
}

func (FfiConverterOobCodeState) Write(writer io.Writer, value OobCodeState) {
	switch variant_value := value.(type) {
	case OobCodeStateIdle:
		writeInt32(writer, 1)
	case OobCodeStateVerifying:
		writeInt32(writer, 2)
	case OobCodeStateValid:
		writeInt32(writer, 3)
		FfiConverterStringINSTANCE.Write(writer, variant_value.InviteId)
		FfiConverterOptionalStringINSTANCE.Write(writer, variant_value.SupervisedBy)
	case OobCodeStateInvalid:
		writeInt32(writer, 4)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Reason)
	case OobCodeStateError:
		writeInt32(writer, 5)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Cause)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterOobCodeState.Write", value))
	}
}

type FfiDestroyerOobCodeState struct{}

func (_ FfiDestroyerOobCodeState) Destroy(value OobCodeState) {
	value.Destroy()
}

// `derive(thiserror::Error)` gives `Display` so this can cross the UniFFI
// boundary as a `Result` error (`OnboardingMachine::probe_setup_status_at` is
// `#[uniffi::export]`); the `uniffi::Error` derive emits the FFI scaffolding.
type ProbeError struct {
	err error
}

// Convenience method to turn *ProbeError into error
// Avoiding treating nil pointer as non nil error interface
func (err *ProbeError) AsError() error {
	if err == nil {
		return nil
	} else {
		return err
	}
}

func (err ProbeError) Error() string {
	return fmt.Sprintf("ProbeError: %s", err.err.Error())
}

func (err ProbeError) Unwrap() error {
	return err.err
}

// Err* are used for checking error type with `errors.Is`
var ErrProbeErrorTransient = fmt.Errorf("ProbeErrorTransient")
var ErrProbeErrorInvalidResponse = fmt.Errorf("ProbeErrorInvalidResponse")
var ErrProbeErrorIdentityMismatch = fmt.Errorf("ProbeErrorIdentityMismatch")

// Variant structs
type ProbeErrorTransient struct {
	Reason string
}

func NewProbeErrorTransient(
	reason string,
) *ProbeError {
	return &ProbeError{err: &ProbeErrorTransient{
		Reason: reason}}
}

func (e ProbeErrorTransient) destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

func (err ProbeErrorTransient) Error() string {
	return fmt.Sprint("Transient",
		": ",

		"Reason=",
		err.Reason,
	)
}

func (self ProbeErrorTransient) Is(target error) bool {
	return target == ErrProbeErrorTransient
}

type ProbeErrorInvalidResponse struct {
	Reason string
}

func NewProbeErrorInvalidResponse(
	reason string,
) *ProbeError {
	return &ProbeError{err: &ProbeErrorInvalidResponse{
		Reason: reason}}
}

func (e ProbeErrorInvalidResponse) destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

func (err ProbeErrorInvalidResponse) Error() string {
	return fmt.Sprint("InvalidResponse",
		": ",

		"Reason=",
		err.Reason,
	)
}

func (self ProbeErrorInvalidResponse) Is(target error) bool {
	return target == ErrProbeErrorInvalidResponse
}

// The connect-stage first-contact trust graduation failed with an
// identity at stake — a held first-contact root, or a changed TOFU pin
// (`security.md` § Pre-claim surfacing). Terminal: never retried, never
// rendered as a transient — the "Almost ready" poll and the provisioning
// claim substep stop on it instead of resting on the DNS message.
type ProbeErrorIdentityMismatch struct {
	Reason string
}

// The connect-stage first-contact trust graduation failed with an
// identity at stake — a held first-contact root, or a changed TOFU pin
// (`security.md` § Pre-claim surfacing). Terminal: never retried, never
// rendered as a transient — the "Almost ready" poll and the provisioning
// claim substep stop on it instead of resting on the DNS message.
func NewProbeErrorIdentityMismatch(
	reason string,
) *ProbeError {
	return &ProbeError{err: &ProbeErrorIdentityMismatch{
		Reason: reason}}
}

func (e ProbeErrorIdentityMismatch) destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

func (err ProbeErrorIdentityMismatch) Error() string {
	return fmt.Sprint("IdentityMismatch",
		": ",

		"Reason=",
		err.Reason,
	)
}

func (self ProbeErrorIdentityMismatch) Is(target error) bool {
	return target == ErrProbeErrorIdentityMismatch
}

type FfiConverterProbeError struct{}

var FfiConverterProbeErrorINSTANCE = FfiConverterProbeError{}

func (c FfiConverterProbeError) Lift(eb RustBufferI) *ProbeError {
	return LiftFromRustBuffer[*ProbeError](c, eb)
}

func (c FfiConverterProbeError) Lower(value *ProbeError) C.RustBuffer {
	return LowerIntoRustBuffer[*ProbeError](c, value)
}

func (c FfiConverterProbeError) LowerExternal(value *ProbeError) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*ProbeError](c, value))
}

func (c FfiConverterProbeError) Read(reader io.Reader) *ProbeError {
	errorID := readUint32(reader)

	switch errorID {
	case 1:
		return &ProbeError{&ProbeErrorTransient{
			Reason: FfiConverterStringINSTANCE.Read(reader),
		}}
	case 2:
		return &ProbeError{&ProbeErrorInvalidResponse{
			Reason: FfiConverterStringINSTANCE.Read(reader),
		}}
	case 3:
		return &ProbeError{&ProbeErrorIdentityMismatch{
			Reason: FfiConverterStringINSTANCE.Read(reader),
		}}
	default:
		panic(fmt.Sprintf("Unknown error code %d in FfiConverterProbeError.Read()", errorID))
	}
}

func (c FfiConverterProbeError) Write(writer io.Writer, value *ProbeError) {
	switch variantValue := value.err.(type) {
	case *ProbeErrorTransient:
		writeInt32(writer, 1)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Reason)
	case *ProbeErrorInvalidResponse:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Reason)
	case *ProbeErrorIdentityMismatch:
		writeInt32(writer, 3)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Reason)
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiConverterProbeError.Write", value))
	}
}

type FfiDestroyerProbeError struct{}

func (_ FfiDestroyerProbeError) Destroy(value *ProbeError) {
	switch variantValue := value.err.(type) {
	case ProbeErrorTransient:
		variantValue.destroy()
	case ProbeErrorInvalidResponse:
		variantValue.destroy()
	case ProbeErrorIdentityMismatch:
		variantValue.destroy()
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiDestroyerProbeError.Destroy", value))
	}
}

// Per-provider DNS-config status surfaced to the wizard's UI. Computed
// by `OnboardingMachine::provider_status()` from already-in-state
// inputs; pure function, no IO.
type ProviderStatus interface {
	Destroy()
}

// No provider selected, or `verify_dns` hasn't completed yet.
// UI: hide status text. Continue disabled.
type ProviderStatusNotReady struct {
}

func (e ProviderStatusNotReady) Destroy() {
}

// Provider's DNS account already hosts this handle's zone.
// UI: "you own this domain at <provider>". Continue enabled.
type ProviderStatusProviderHasDomain struct {
}

func (e ProviderStatusProviderHasDomain) Destroy() {
}

// Domain is registered, zone NOT in this provider's zone list.
// UI: "transfer this domain to <provider> manually first."
// Continue disabled.
type ProviderStatusRegisteredElsewhere struct {
}

func (e ProviderStatusRegisteredElsewhere) Destroy() {
}

// Domain unregistered AND `Registrar::availability()` returned
// `Buyable`. UI shows price + price-confirm checkbox + (per
// `requires_contact()`) the WHOIS form. Continue enabled iff
// `price_agreed && (!requires_contact || contact.is_some())`.
type ProviderStatusUnregisteredBuyable struct {
	PriceCents uint64
	Currency   *string
}

func (e ProviderStatusUnregisteredBuyable) Destroy() {
	FfiDestroyerUint64{}.Destroy(e.PriceCents)
	FfiDestroyerOptionalString{}.Destroy(e.Currency)
}

// Domain unregistered AND no buyable signal — provider has no
// Registrar capability, or `availability()` returned `Unavailable`
// / `TldNotSupported`. UI: "<provider> can't sell this — pick
// another or buy elsewhere."
type ProviderStatusUnregisteredNotBuyable struct {
}

func (e ProviderStatusUnregisteredNotBuyable) Destroy() {
}

type FfiConverterProviderStatus struct{}

var FfiConverterProviderStatusINSTANCE = FfiConverterProviderStatus{}

func (c FfiConverterProviderStatus) Lift(rb RustBufferI) ProviderStatus {
	return LiftFromRustBuffer[ProviderStatus](c, rb)
}

func (c FfiConverterProviderStatus) Lower(value ProviderStatus) C.RustBuffer {
	return LowerIntoRustBuffer[ProviderStatus](c, value)
}

func (c FfiConverterProviderStatus) LowerExternal(value ProviderStatus) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ProviderStatus](c, value))
}
func (FfiConverterProviderStatus) Read(reader io.Reader) ProviderStatus {
	id := readInt32(reader)
	switch id {
	case 1:
		return ProviderStatusNotReady{}
	case 2:
		return ProviderStatusProviderHasDomain{}
	case 3:
		return ProviderStatusRegisteredElsewhere{}
	case 4:
		return ProviderStatusUnregisteredBuyable{
			FfiConverterUint64INSTANCE.Read(reader),
			FfiConverterOptionalStringINSTANCE.Read(reader),
		}
	case 5:
		return ProviderStatusUnregisteredNotBuyable{}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterProviderStatus.Read()", id))
	}
}

func (FfiConverterProviderStatus) Write(writer io.Writer, value ProviderStatus) {
	switch variant_value := value.(type) {
	case ProviderStatusNotReady:
		writeInt32(writer, 1)
	case ProviderStatusProviderHasDomain:
		writeInt32(writer, 2)
	case ProviderStatusRegisteredElsewhere:
		writeInt32(writer, 3)
	case ProviderStatusUnregisteredBuyable:
		writeInt32(writer, 4)
		FfiConverterUint64INSTANCE.Write(writer, variant_value.PriceCents)
		FfiConverterOptionalStringINSTANCE.Write(writer, variant_value.Currency)
	case ProviderStatusUnregisteredNotBuyable:
		writeInt32(writer, 5)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterProviderStatus.Write", value))
	}
}

type FfiDestroyerProviderStatus struct{}

func (_ FfiDestroyerProviderStatus) Destroy(value ProviderStatus) {
	value.Destroy()
}

// What [`OnboardingMachine::submit_recovery_entry`] produced.
//
// One variant per thing the user can do next, which is what makes it narrower
// than the transport's own [`crate::nest_api::RestoreSeedError`]. Each maps to
// an `onboarding.recovery_entry.*` i18n key the **client** resolves — the
// machine holds no string table, the same division the handle-check's
// `LocalizedText` snapshots use.
// The `uniffi::Enum` derive is **mandatory, not optional**: this is the return
// type of `submit_recovery_entry`, which lives in the `#[uniffi::export]`ed
// `impl OnboardingMachine` block, so without it the whole `uniffi`-feature
// build fails (`LowerReturn`/`Lift`/`TypeId` unsatisfied) and takes the
// mail-bridge and apple FFI gates with it. Verify with
// `cargo check -p fauna-onboarding-machine --features uniffi` — the
// default-feature build says nothing about it.
type RecoveryEntryOutcome interface {
	Destroy()
}

// The seed is restored and the wizard is on `handle_entry`, with the
// account carried into the handle field.
type RecoveryEntryOutcomeRestored struct {
}

func (e RecoveryEntryOutcomeRestored) Destroy() {
}

// [`Self::Restored`] — the account **is** back and the wizard advanced
// identically — but the blob's predecessor section was present and did not
// open, so a corpus still sealed under a predecessor identity is now
// unopenable (`identity-succession.md` § Seed escrow). Key
// `restored_predecessors_lost` (arg `reason`).
//
// A separate variant rather than a flag on `Restored` because a client
// must not be able to render the success path without deciding what to say
// here: this is the one place a *successful* recovery still lost
// user-irrecoverable data, and silence is the failure mode that matters.
type RecoveryEntryOutcomeRestoredPredecessorsLost struct {
	Reason string
}

func (e RecoveryEntryOutcomeRestoredPredecessorsLost) Destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

// The phrase is not a recovery kit — refused by the local parse, so
// nothing was sent. Key `invalid_kit`.
type RecoveryEntryOutcomeInvalidKit struct {
}

func (e RecoveryEntryOutcomeInvalidKit) Destroy() {
}

// Nothing named the account: the payload carried no handle and the
// account field was empty. Key `account_needed`.
type RecoveryEntryOutcomeAccountNeeded struct {
}

func (e RecoveryEntryOutcomeAccountNeeded) Destroy() {
}

// The account was named but is not `user@domain`, so there is no domain to
// find a nest at. Key `account_malformed`.
type RecoveryEntryOutcomeAccountMalformed struct {
}

func (e RecoveryEntryOutcomeAccountMalformed) Destroy() {
}

// The nest was reached and knows no such account. Key `account_unknown`
// (arg `account`).
type RecoveryEntryOutcomeAccountUnknown struct {
	Account string
}

func (e RecoveryEntryOutcomeAccountUnknown) Destroy() {
	FfiDestroyerString{}.Destroy(e.Account)
}

// No escrow blob rests for this account — the ratified honest signal, not
// a fault. Key `no_escrow`.
type RecoveryEntryOutcomeNoEscrow struct {
}

func (e RecoveryEntryOutcomeNoEscrow) Destroy() {
}

// The identity was succeeded. The **client** routes this to
// [`OnboardingMachine::begin_import_identity_with_reason`] with the
// `superseded` string, uniform with the launch flow's refusal; the
// successor is deliberately not carried, because it arrives unverified.
type RecoveryEntryOutcomeSuperseded struct {
}

func (e RecoveryEntryOutcomeSuperseded) Destroy() {
}

// The nest declined the kit — replaced, retired, or another account's.
// Key `refused` (arg `reason`).
type RecoveryEntryOutcomeRefused struct {
	Reason string
}

func (e RecoveryEntryOutcomeRefused) Destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

// The nest could not be reached or resolved; a retry is worth offering.
// Key `unreachable` (arg `reason`).
type RecoveryEntryOutcomeUnreachable struct {
	Reason string
}

func (e RecoveryEntryOutcomeUnreachable) Destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

type FfiConverterRecoveryEntryOutcome struct{}

var FfiConverterRecoveryEntryOutcomeINSTANCE = FfiConverterRecoveryEntryOutcome{}

func (c FfiConverterRecoveryEntryOutcome) Lift(rb RustBufferI) RecoveryEntryOutcome {
	return LiftFromRustBuffer[RecoveryEntryOutcome](c, rb)
}

func (c FfiConverterRecoveryEntryOutcome) Lower(value RecoveryEntryOutcome) C.RustBuffer {
	return LowerIntoRustBuffer[RecoveryEntryOutcome](c, value)
}

func (c FfiConverterRecoveryEntryOutcome) LowerExternal(value RecoveryEntryOutcome) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RecoveryEntryOutcome](c, value))
}
func (FfiConverterRecoveryEntryOutcome) Read(reader io.Reader) RecoveryEntryOutcome {
	id := readInt32(reader)
	switch id {
	case 1:
		return RecoveryEntryOutcomeRestored{}
	case 2:
		return RecoveryEntryOutcomeRestoredPredecessorsLost{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 3:
		return RecoveryEntryOutcomeInvalidKit{}
	case 4:
		return RecoveryEntryOutcomeAccountNeeded{}
	case 5:
		return RecoveryEntryOutcomeAccountMalformed{}
	case 6:
		return RecoveryEntryOutcomeAccountUnknown{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 7:
		return RecoveryEntryOutcomeNoEscrow{}
	case 8:
		return RecoveryEntryOutcomeSuperseded{}
	case 9:
		return RecoveryEntryOutcomeRefused{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 10:
		return RecoveryEntryOutcomeUnreachable{
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterRecoveryEntryOutcome.Read()", id))
	}
}

func (FfiConverterRecoveryEntryOutcome) Write(writer io.Writer, value RecoveryEntryOutcome) {
	switch variant_value := value.(type) {
	case RecoveryEntryOutcomeRestored:
		writeInt32(writer, 1)
	case RecoveryEntryOutcomeRestoredPredecessorsLost:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Reason)
	case RecoveryEntryOutcomeInvalidKit:
		writeInt32(writer, 3)
	case RecoveryEntryOutcomeAccountNeeded:
		writeInt32(writer, 4)
	case RecoveryEntryOutcomeAccountMalformed:
		writeInt32(writer, 5)
	case RecoveryEntryOutcomeAccountUnknown:
		writeInt32(writer, 6)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Account)
	case RecoveryEntryOutcomeNoEscrow:
		writeInt32(writer, 7)
	case RecoveryEntryOutcomeSuperseded:
		writeInt32(writer, 8)
	case RecoveryEntryOutcomeRefused:
		writeInt32(writer, 9)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Reason)
	case RecoveryEntryOutcomeUnreachable:
		writeInt32(writer, 10)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Reason)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterRecoveryEntryOutcome.Write", value))
	}
}

type FfiDestroyerRecoveryEntryOutcome struct{}

func (_ FfiDestroyerRecoveryEntryOutcome) Destroy(value RecoveryEntryOutcome) {
	value.Destroy()
}

// Page states, in the order the view walks them (§ Layout & flow).
type RetirePhase uint

const (
	// The `vps_config` provider row, credential form and verify button,
	// reused by ID — no region or server-type controls.
	RetirePhaseCredentials RetirePhase = 1
	// `verify` + `list_managed_servers` in flight.
	RetirePhaseListing RetirePhase = 2
	// One row per listed server.
	RetirePhaseList RetirePhase = 3
	// Type-the-name confirm.
	RetirePhaseConfirm RetirePhase = 4
	// The two step rows: DNS, then Server.
	RetirePhaseRunning RetirePhase = 5
	// Outcome, plus any DNS left to remove by hand.
	RetirePhaseDone RetirePhase = 6
)

type FfiConverterRetirePhase struct{}

var FfiConverterRetirePhaseINSTANCE = FfiConverterRetirePhase{}

func (c FfiConverterRetirePhase) Lift(rb RustBufferI) RetirePhase {
	return LiftFromRustBuffer[RetirePhase](c, rb)
}

func (c FfiConverterRetirePhase) Lower(value RetirePhase) C.RustBuffer {
	return LowerIntoRustBuffer[RetirePhase](c, value)
}

func (c FfiConverterRetirePhase) LowerExternal(value RetirePhase) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RetirePhase](c, value))
}
func (FfiConverterRetirePhase) Read(reader io.Reader) RetirePhase {
	id := readInt32(reader)
	return RetirePhase(id)
}

func (FfiConverterRetirePhase) Write(writer io.Writer, value RetirePhase) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerRetirePhase struct{}

func (_ FfiDestroyerRetirePhase) Destroy(value RetirePhase) {
}

// The two steps of a retire run, in order.
type RetireStep uint

const (
	RetireStepDns    RetireStep = 1
	RetireStepServer RetireStep = 2
)

type FfiConverterRetireStep struct{}

var FfiConverterRetireStepINSTANCE = FfiConverterRetireStep{}

func (c FfiConverterRetireStep) Lift(rb RustBufferI) RetireStep {
	return LiftFromRustBuffer[RetireStep](c, rb)
}

func (c FfiConverterRetireStep) Lower(value RetireStep) C.RustBuffer {
	return LowerIntoRustBuffer[RetireStep](c, value)
}

func (c FfiConverterRetireStep) LowerExternal(value RetireStep) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RetireStep](c, value))
}
func (FfiConverterRetireStep) Read(reader io.Reader) RetireStep {
	id := readInt32(reader)
	return RetireStep(id)
}

func (FfiConverterRetireStep) Write(writer io.Writer, value RetireStep) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerRetireStep struct{}

func (_ FfiDestroyerRetireStep) Destroy(value RetireStep) {
}

type StepState interface {
	Destroy()
}
type StepStatePending struct {
}

func (e StepStatePending) Destroy() {
}

type StepStateRunning struct {
}

func (e StepStateRunning) Destroy() {
}

type StepStateDone struct {
}

func (e StepStateDone) Destroy() {
}

// The step could not run at all — no verified domain, or no usable DNS
// credential. Not a failure: the run continues to the server.
type StepStateSkipped struct {
	Why string
}

func (e StepStateSkipped) Destroy() {
	FfiDestroyerString{}.Destroy(e.Why)
}

type StepStateFailed struct {
	Cause string
}

func (e StepStateFailed) Destroy() {
	FfiDestroyerString{}.Destroy(e.Cause)
}

type FfiConverterStepState struct{}

var FfiConverterStepStateINSTANCE = FfiConverterStepState{}

func (c FfiConverterStepState) Lift(rb RustBufferI) StepState {
	return LiftFromRustBuffer[StepState](c, rb)
}

func (c FfiConverterStepState) Lower(value StepState) C.RustBuffer {
	return LowerIntoRustBuffer[StepState](c, value)
}

func (c FfiConverterStepState) LowerExternal(value StepState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[StepState](c, value))
}
func (FfiConverterStepState) Read(reader io.Reader) StepState {
	id := readInt32(reader)
	switch id {
	case 1:
		return StepStatePending{}
	case 2:
		return StepStateRunning{}
	case 3:
		return StepStateDone{}
	case 4:
		return StepStateSkipped{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 5:
		return StepStateFailed{
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterStepState.Read()", id))
	}
}

func (FfiConverterStepState) Write(writer io.Writer, value StepState) {
	switch variant_value := value.(type) {
	case StepStatePending:
		writeInt32(writer, 1)
	case StepStateRunning:
		writeInt32(writer, 2)
	case StepStateDone:
		writeInt32(writer, 3)
	case StepStateSkipped:
		writeInt32(writer, 4)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Why)
	case StepStateFailed:
		writeInt32(writer, 5)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Cause)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterStepState.Write", value))
	}
}

type FfiDestroyerStepState struct{}

func (_ FfiDestroyerStepState) Destroy(value StepState) {
	value.Destroy()
}

// The transfer-authorization-code affordance on a selected row
// (§ Transfer authorization code).
type TransferCodeState interface {
	Destroy()
}

// This provider has no registrar adapter with an auth-code call, or the
// row has no verified domain. The row carries a one-line note instead of
// a button — never a button that would fail.
type TransferCodeStateUnsupported struct {
}

func (e TransferCodeStateUnsupported) Destroy() {
}

// Supported and not yet asked for.
type TransferCodeStateIdle struct {
}

func (e TransferCodeStateIdle) Destroy() {
}

// The call is in flight.
type TransferCodeStateFetching struct {
}

func (e TransferCodeStateFetching) Destroy() {
}

// The code, held in memory only and cleared on page exit.
type TransferCodeStateCode struct {
	Code string
}

func (e TransferCodeStateCode) Destroy() {
	FfiDestroyerString{}.Destroy(e.Code)
}

// A registry transfer lock still applies; this is when it lifts. Never a
// refusal (`bundled-provider-api.md` § Exit guarantee 1).
type TransferCodeStateAvailableAfter struct {
	When string
}

func (e TransferCodeStateAvailableAfter) Destroy() {
	FfiDestroyerString{}.Destroy(e.When)
}

type FfiConverterTransferCodeState struct{}

var FfiConverterTransferCodeStateINSTANCE = FfiConverterTransferCodeState{}

func (c FfiConverterTransferCodeState) Lift(rb RustBufferI) TransferCodeState {
	return LiftFromRustBuffer[TransferCodeState](c, rb)
}

func (c FfiConverterTransferCodeState) Lower(value TransferCodeState) C.RustBuffer {
	return LowerIntoRustBuffer[TransferCodeState](c, value)
}

func (c FfiConverterTransferCodeState) LowerExternal(value TransferCodeState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[TransferCodeState](c, value))
}
func (FfiConverterTransferCodeState) Read(reader io.Reader) TransferCodeState {
	id := readInt32(reader)
	switch id {
	case 1:
		return TransferCodeStateUnsupported{}
	case 2:
		return TransferCodeStateIdle{}
	case 3:
		return TransferCodeStateFetching{}
	case 4:
		return TransferCodeStateCode{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 5:
		return TransferCodeStateAvailableAfter{
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterTransferCodeState.Read()", id))
	}
}

func (FfiConverterTransferCodeState) Write(writer io.Writer, value TransferCodeState) {
	switch variant_value := value.(type) {
	case TransferCodeStateUnsupported:
		writeInt32(writer, 1)
	case TransferCodeStateIdle:
		writeInt32(writer, 2)
	case TransferCodeStateFetching:
		writeInt32(writer, 3)
	case TransferCodeStateCode:
		writeInt32(writer, 4)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Code)
	case TransferCodeStateAvailableAfter:
		writeInt32(writer, 5)
		FfiConverterStringINSTANCE.Write(writer, variant_value.When)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterTransferCodeState.Write", value))
	}
}

type FfiDestroyerTransferCodeState struct{}

func (_ FfiDestroyerTransferCodeState) Destroy(value TransferCodeState) {
	value.Destroy()
}

type WizardOutcome interface {
	Destroy()
}
type WizardOutcomeLoggedIn struct {
	NestUrl string
	Handle  string
}

func (e WizardOutcomeLoggedIn) Destroy() {
	FfiDestroyerString{}.Destroy(e.NestUrl)
	FfiDestroyerString{}.Destroy(e.Handle)
}

type WizardOutcomeAwaitingManualDns struct {
	NestUrl    string
	DnsRecords []DnsRecordPlain
	ClaimCode  string
}

func (e WizardOutcomeAwaitingManualDns) Destroy() {
	FfiDestroyerString{}.Destroy(e.NestUrl)
	FfiDestroyerSequenceDnsRecordPlain{}.Destroy(e.DnsRecords)
	FfiDestroyerString{}.Destroy(e.ClaimCode)
}

type FfiConverterWizardOutcome struct{}

var FfiConverterWizardOutcomeINSTANCE = FfiConverterWizardOutcome{}

func (c FfiConverterWizardOutcome) Lift(rb RustBufferI) WizardOutcome {
	return LiftFromRustBuffer[WizardOutcome](c, rb)
}

func (c FfiConverterWizardOutcome) Lower(value WizardOutcome) C.RustBuffer {
	return LowerIntoRustBuffer[WizardOutcome](c, value)
}

func (c FfiConverterWizardOutcome) LowerExternal(value WizardOutcome) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[WizardOutcome](c, value))
}
func (FfiConverterWizardOutcome) Read(reader io.Reader) WizardOutcome {
	id := readInt32(reader)
	switch id {
	case 1:
		return WizardOutcomeLoggedIn{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 2:
		return WizardOutcomeAwaitingManualDns{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterSequenceDnsRecordPlainINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterWizardOutcome.Read()", id))
	}
}

func (FfiConverterWizardOutcome) Write(writer io.Writer, value WizardOutcome) {
	switch variant_value := value.(type) {
	case WizardOutcomeLoggedIn:
		writeInt32(writer, 1)
		FfiConverterStringINSTANCE.Write(writer, variant_value.NestUrl)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Handle)
	case WizardOutcomeAwaitingManualDns:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variant_value.NestUrl)
		FfiConverterSequenceDnsRecordPlainINSTANCE.Write(writer, variant_value.DnsRecords)
		FfiConverterStringINSTANCE.Write(writer, variant_value.ClaimCode)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterWizardOutcome.Write", value))
	}
}

type FfiDestroyerWizardOutcome struct{}

func (_ FfiDestroyerWizardOutcome) Destroy(value WizardOutcome) {
	value.Destroy()
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

type FfiConverterOptionalBool struct{}

var FfiConverterOptionalBoolINSTANCE = FfiConverterOptionalBool{}

func (c FfiConverterOptionalBool) Lift(rb RustBufferI) *bool {
	return LiftFromRustBuffer[*bool](c, rb)
}

func (_ FfiConverterOptionalBool) Read(reader io.Reader) *bool {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterBoolINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalBool) Lower(value *bool) C.RustBuffer {
	return LowerIntoRustBuffer[*bool](c, value)
}

func (c FfiConverterOptionalBool) LowerExternal(value *bool) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*bool](c, value))
}

func (_ FfiConverterOptionalBool) Write(writer io.Writer, value *bool) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterBoolINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalBool struct{}

func (_ FfiDestroyerOptionalBool) Destroy(value *bool) {
	if value != nil {
		FfiDestroyerBool{}.Destroy(*value)
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

type FfiConverterOptionalAgeAttestationPlain struct{}

var FfiConverterOptionalAgeAttestationPlainINSTANCE = FfiConverterOptionalAgeAttestationPlain{}

func (c FfiConverterOptionalAgeAttestationPlain) Lift(rb RustBufferI) *AgeAttestationPlain {
	return LiftFromRustBuffer[*AgeAttestationPlain](c, rb)
}

func (_ FfiConverterOptionalAgeAttestationPlain) Read(reader io.Reader) *AgeAttestationPlain {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterAgeAttestationPlainINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalAgeAttestationPlain) Lower(value *AgeAttestationPlain) C.RustBuffer {
	return LowerIntoRustBuffer[*AgeAttestationPlain](c, value)
}

func (c FfiConverterOptionalAgeAttestationPlain) LowerExternal(value *AgeAttestationPlain) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*AgeAttestationPlain](c, value))
}

func (_ FfiConverterOptionalAgeAttestationPlain) Write(writer io.Writer, value *AgeAttestationPlain) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterAgeAttestationPlainINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalAgeAttestationPlain struct{}

func (_ FfiDestroyerOptionalAgeAttestationPlain) Destroy(value *AgeAttestationPlain) {
	if value != nil {
		FfiDestroyerAgeAttestationPlain{}.Destroy(*value)
	}
}

type FfiConverterOptionalAgeClaimPlain struct{}

var FfiConverterOptionalAgeClaimPlainINSTANCE = FfiConverterOptionalAgeClaimPlain{}

func (c FfiConverterOptionalAgeClaimPlain) Lift(rb RustBufferI) *AgeClaimPlain {
	return LiftFromRustBuffer[*AgeClaimPlain](c, rb)
}

func (_ FfiConverterOptionalAgeClaimPlain) Read(reader io.Reader) *AgeClaimPlain {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterAgeClaimPlainINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalAgeClaimPlain) Lower(value *AgeClaimPlain) C.RustBuffer {
	return LowerIntoRustBuffer[*AgeClaimPlain](c, value)
}

func (c FfiConverterOptionalAgeClaimPlain) LowerExternal(value *AgeClaimPlain) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*AgeClaimPlain](c, value))
}

func (_ FfiConverterOptionalAgeClaimPlain) Write(writer io.Writer, value *AgeClaimPlain) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterAgeClaimPlainINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalAgeClaimPlain struct{}

func (_ FfiDestroyerOptionalAgeClaimPlain) Destroy(value *AgeClaimPlain) {
	if value != nil {
		FfiDestroyerAgeClaimPlain{}.Destroy(*value)
	}
}

type FfiConverterOptionalCapturedDnsCredential struct{}

var FfiConverterOptionalCapturedDnsCredentialINSTANCE = FfiConverterOptionalCapturedDnsCredential{}

func (c FfiConverterOptionalCapturedDnsCredential) Lift(rb RustBufferI) *CapturedDnsCredential {
	return LiftFromRustBuffer[*CapturedDnsCredential](c, rb)
}

func (_ FfiConverterOptionalCapturedDnsCredential) Read(reader io.Reader) *CapturedDnsCredential {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterCapturedDnsCredentialINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalCapturedDnsCredential) Lower(value *CapturedDnsCredential) C.RustBuffer {
	return LowerIntoRustBuffer[*CapturedDnsCredential](c, value)
}

func (c FfiConverterOptionalCapturedDnsCredential) LowerExternal(value *CapturedDnsCredential) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*CapturedDnsCredential](c, value))
}

func (_ FfiConverterOptionalCapturedDnsCredential) Write(writer io.Writer, value *CapturedDnsCredential) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterCapturedDnsCredentialINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalCapturedDnsCredential struct{}

func (_ FfiDestroyerOptionalCapturedDnsCredential) Destroy(value *CapturedDnsCredential) {
	if value != nil {
		FfiDestroyerCapturedDnsCredential{}.Destroy(*value)
	}
}

type FfiConverterOptionalHostedAuthPrompt struct{}

var FfiConverterOptionalHostedAuthPromptINSTANCE = FfiConverterOptionalHostedAuthPrompt{}

func (c FfiConverterOptionalHostedAuthPrompt) Lift(rb RustBufferI) *HostedAuthPrompt {
	return LiftFromRustBuffer[*HostedAuthPrompt](c, rb)
}

func (_ FfiConverterOptionalHostedAuthPrompt) Read(reader io.Reader) *HostedAuthPrompt {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterHostedAuthPromptINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalHostedAuthPrompt) Lower(value *HostedAuthPrompt) C.RustBuffer {
	return LowerIntoRustBuffer[*HostedAuthPrompt](c, value)
}

func (c FfiConverterOptionalHostedAuthPrompt) LowerExternal(value *HostedAuthPrompt) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*HostedAuthPrompt](c, value))
}

func (_ FfiConverterOptionalHostedAuthPrompt) Write(writer io.Writer, value *HostedAuthPrompt) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterHostedAuthPromptINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalHostedAuthPrompt struct{}

func (_ FfiDestroyerOptionalHostedAuthPrompt) Destroy(value *HostedAuthPrompt) {
	if value != nil {
		FfiDestroyerHostedAuthPrompt{}.Destroy(*value)
	}
}

type FfiConverterOptionalManagedServerRow struct{}

var FfiConverterOptionalManagedServerRowINSTANCE = FfiConverterOptionalManagedServerRow{}

func (c FfiConverterOptionalManagedServerRow) Lift(rb RustBufferI) *ManagedServerRow {
	return LiftFromRustBuffer[*ManagedServerRow](c, rb)
}

func (_ FfiConverterOptionalManagedServerRow) Read(reader io.Reader) *ManagedServerRow {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterManagedServerRowINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalManagedServerRow) Lower(value *ManagedServerRow) C.RustBuffer {
	return LowerIntoRustBuffer[*ManagedServerRow](c, value)
}

func (c FfiConverterOptionalManagedServerRow) LowerExternal(value *ManagedServerRow) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*ManagedServerRow](c, value))
}

func (_ FfiConverterOptionalManagedServerRow) Write(writer io.Writer, value *ManagedServerRow) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterManagedServerRowINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalManagedServerRow struct{}

func (_ FfiDestroyerOptionalManagedServerRow) Destroy(value *ManagedServerRow) {
	if value != nil {
		FfiDestroyerManagedServerRow{}.Destroy(*value)
	}
}

type FfiConverterOptionalPendingInviteSlot struct{}

var FfiConverterOptionalPendingInviteSlotINSTANCE = FfiConverterOptionalPendingInviteSlot{}

func (c FfiConverterOptionalPendingInviteSlot) Lift(rb RustBufferI) *PendingInviteSlot {
	return LiftFromRustBuffer[*PendingInviteSlot](c, rb)
}

func (_ FfiConverterOptionalPendingInviteSlot) Read(reader io.Reader) *PendingInviteSlot {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterPendingInviteSlotINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalPendingInviteSlot) Lower(value *PendingInviteSlot) C.RustBuffer {
	return LowerIntoRustBuffer[*PendingInviteSlot](c, value)
}

func (c FfiConverterOptionalPendingInviteSlot) LowerExternal(value *PendingInviteSlot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*PendingInviteSlot](c, value))
}

func (_ FfiConverterOptionalPendingInviteSlot) Write(writer io.Writer, value *PendingInviteSlot) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterPendingInviteSlotINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalPendingInviteSlot struct{}

func (_ FfiDestroyerOptionalPendingInviteSlot) Destroy(value *PendingInviteSlot) {
	if value != nil {
		FfiDestroyerPendingInviteSlot{}.Destroy(*value)
	}
}

type FfiConverterOptionalContactInfo struct{}

var FfiConverterOptionalContactInfoINSTANCE = FfiConverterOptionalContactInfo{}

func (c FfiConverterOptionalContactInfo) Lift(rb RustBufferI) *fauna_provisioning.ContactInfo {
	return LiftFromRustBuffer[*fauna_provisioning.ContactInfo](c, rb)
}

func (_ FfiConverterOptionalContactInfo) Read(reader io.Reader) *fauna_provisioning.ContactInfo {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := fauna_provisioning.FfiConverterContactInfoINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalContactInfo) Lower(value *fauna_provisioning.ContactInfo) C.RustBuffer {
	return LowerIntoRustBuffer[*fauna_provisioning.ContactInfo](c, value)
}

func (c FfiConverterOptionalContactInfo) LowerExternal(value *fauna_provisioning.ContactInfo) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*fauna_provisioning.ContactInfo](c, value))
}

func (_ FfiConverterOptionalContactInfo) Write(writer io.Writer, value *fauna_provisioning.ContactInfo) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		fauna_provisioning.FfiConverterContactInfoINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalContactInfo struct{}

func (_ FfiDestroyerOptionalContactInfo) Destroy(value *fauna_provisioning.ContactInfo) {
	if value != nil {
		fauna_provisioning.FfiDestroyerContactInfo{}.Destroy(*value)
	}
}

type FfiConverterOptionalTldPriceQuote struct{}

var FfiConverterOptionalTldPriceQuoteINSTANCE = FfiConverterOptionalTldPriceQuote{}

func (c FfiConverterOptionalTldPriceQuote) Lift(rb RustBufferI) *fauna_provisioning.TldPriceQuote {
	return LiftFromRustBuffer[*fauna_provisioning.TldPriceQuote](c, rb)
}

func (_ FfiConverterOptionalTldPriceQuote) Read(reader io.Reader) *fauna_provisioning.TldPriceQuote {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := fauna_provisioning.FfiConverterTldPriceQuoteINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalTldPriceQuote) Lower(value *fauna_provisioning.TldPriceQuote) C.RustBuffer {
	return LowerIntoRustBuffer[*fauna_provisioning.TldPriceQuote](c, value)
}

func (c FfiConverterOptionalTldPriceQuote) LowerExternal(value *fauna_provisioning.TldPriceQuote) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*fauna_provisioning.TldPriceQuote](c, value))
}

func (_ FfiConverterOptionalTldPriceQuote) Write(writer io.Writer, value *fauna_provisioning.TldPriceQuote) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		fauna_provisioning.FfiConverterTldPriceQuoteINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalTldPriceQuote struct{}

func (_ FfiDestroyerOptionalTldPriceQuote) Destroy(value *fauna_provisioning.TldPriceQuote) {
	if value != nil {
		fauna_provisioning.FfiDestroyerTldPriceQuote{}.Destroy(*value)
	}
}

type FfiConverterOptionalNodeMode struct{}

var FfiConverterOptionalNodeModeINSTANCE = FfiConverterOptionalNodeMode{}

func (c FfiConverterOptionalNodeMode) Lift(rb RustBufferI) *fauna_core.NodeMode {
	return LiftFromRustBuffer[*fauna_core.NodeMode](c, rb)
}

func (_ FfiConverterOptionalNodeMode) Read(reader io.Reader) *fauna_core.NodeMode {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := fauna_core.FfiConverterNodeModeINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalNodeMode) Lower(value *fauna_core.NodeMode) C.RustBuffer {
	return LowerIntoRustBuffer[*fauna_core.NodeMode](c, value)
}

func (c FfiConverterOptionalNodeMode) LowerExternal(value *fauna_core.NodeMode) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*fauna_core.NodeMode](c, value))
}

func (_ FfiConverterOptionalNodeMode) Write(writer io.Writer, value *fauna_core.NodeMode) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		fauna_core.FfiConverterNodeModeINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalNodeMode struct{}

func (_ FfiDestroyerOptionalNodeMode) Destroy(value *fauna_core.NodeMode) {
	if value != nil {
		fauna_core.FfiDestroyerNodeMode{}.Destroy(*value)
	}
}

type FfiConverterOptionalBoxRecoveryEntry struct{}

var FfiConverterOptionalBoxRecoveryEntryINSTANCE = FfiConverterOptionalBoxRecoveryEntry{}

func (c FfiConverterOptionalBoxRecoveryEntry) Lift(rb RustBufferI) *BoxRecoveryEntry {
	return LiftFromRustBuffer[*BoxRecoveryEntry](c, rb)
}

func (_ FfiConverterOptionalBoxRecoveryEntry) Read(reader io.Reader) *BoxRecoveryEntry {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterBoxRecoveryEntryINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalBoxRecoveryEntry) Lower(value *BoxRecoveryEntry) C.RustBuffer {
	return LowerIntoRustBuffer[*BoxRecoveryEntry](c, value)
}

func (c FfiConverterOptionalBoxRecoveryEntry) LowerExternal(value *BoxRecoveryEntry) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*BoxRecoveryEntry](c, value))
}

func (_ FfiConverterOptionalBoxRecoveryEntry) Write(writer io.Writer, value *BoxRecoveryEntry) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterBoxRecoveryEntryINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalBoxRecoveryEntry struct{}

func (_ FfiDestroyerOptionalBoxRecoveryEntry) Destroy(value *BoxRecoveryEntry) {
	if value != nil {
		FfiDestroyerBoxRecoveryEntry{}.Destroy(*value)
	}
}

type FfiConverterOptionalIdentityOrigin struct{}

var FfiConverterOptionalIdentityOriginINSTANCE = FfiConverterOptionalIdentityOrigin{}

func (c FfiConverterOptionalIdentityOrigin) Lift(rb RustBufferI) *IdentityOrigin {
	return LiftFromRustBuffer[*IdentityOrigin](c, rb)
}

func (_ FfiConverterOptionalIdentityOrigin) Read(reader io.Reader) *IdentityOrigin {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterIdentityOriginINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalIdentityOrigin) Lower(value *IdentityOrigin) C.RustBuffer {
	return LowerIntoRustBuffer[*IdentityOrigin](c, value)
}

func (c FfiConverterOptionalIdentityOrigin) LowerExternal(value *IdentityOrigin) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*IdentityOrigin](c, value))
}

func (_ FfiConverterOptionalIdentityOrigin) Write(writer io.Writer, value *IdentityOrigin) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterIdentityOriginINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalIdentityOrigin struct{}

func (_ FfiDestroyerOptionalIdentityOrigin) Destroy(value *IdentityOrigin) {
	if value != nil {
		FfiDestroyerIdentityOrigin{}.Destroy(*value)
	}
}

type FfiConverterOptionalWizardOutcome struct{}

var FfiConverterOptionalWizardOutcomeINSTANCE = FfiConverterOptionalWizardOutcome{}

func (c FfiConverterOptionalWizardOutcome) Lift(rb RustBufferI) *WizardOutcome {
	return LiftFromRustBuffer[*WizardOutcome](c, rb)
}

func (_ FfiConverterOptionalWizardOutcome) Read(reader io.Reader) *WizardOutcome {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterWizardOutcomeINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalWizardOutcome) Lower(value *WizardOutcome) C.RustBuffer {
	return LowerIntoRustBuffer[*WizardOutcome](c, value)
}

func (c FfiConverterOptionalWizardOutcome) LowerExternal(value *WizardOutcome) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*WizardOutcome](c, value))
}

func (_ FfiConverterOptionalWizardOutcome) Write(writer io.Writer, value *WizardOutcome) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterWizardOutcomeINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalWizardOutcome struct{}

func (_ FfiDestroyerOptionalWizardOutcome) Destroy(value *WizardOutcome) {
	if value != nil {
		FfiDestroyerWizardOutcome{}.Destroy(*value)
	}
}

type FfiConverterOptionalDomainStatus struct{}

var FfiConverterOptionalDomainStatusINSTANCE = FfiConverterOptionalDomainStatus{}

func (c FfiConverterOptionalDomainStatus) Lift(rb RustBufferI) *fauna_provisioning.DomainStatus {
	return LiftFromRustBuffer[*fauna_provisioning.DomainStatus](c, rb)
}

func (_ FfiConverterOptionalDomainStatus) Read(reader io.Reader) *fauna_provisioning.DomainStatus {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := fauna_provisioning.FfiConverterDomainStatusINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalDomainStatus) Lower(value *fauna_provisioning.DomainStatus) C.RustBuffer {
	return LowerIntoRustBuffer[*fauna_provisioning.DomainStatus](c, value)
}

func (c FfiConverterOptionalDomainStatus) LowerExternal(value *fauna_provisioning.DomainStatus) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*fauna_provisioning.DomainStatus](c, value))
}

func (_ FfiConverterOptionalDomainStatus) Write(writer io.Writer, value *fauna_provisioning.DomainStatus) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		fauna_provisioning.FfiConverterDomainStatusINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalDomainStatus struct{}

func (_ FfiDestroyerOptionalDomainStatus) Destroy(value *fauna_provisioning.DomainStatus) {
	if value != nil {
		fauna_provisioning.FfiDestroyerDomainStatus{}.Destroy(*value)
	}
}

type FfiConverterOptionalRegistrarAvailability struct{}

var FfiConverterOptionalRegistrarAvailabilityINSTANCE = FfiConverterOptionalRegistrarAvailability{}

func (c FfiConverterOptionalRegistrarAvailability) Lift(rb RustBufferI) *fauna_provisioning.RegistrarAvailability {
	return LiftFromRustBuffer[*fauna_provisioning.RegistrarAvailability](c, rb)
}

func (_ FfiConverterOptionalRegistrarAvailability) Read(reader io.Reader) *fauna_provisioning.RegistrarAvailability {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := fauna_provisioning.FfiConverterRegistrarAvailabilityINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalRegistrarAvailability) Lower(value *fauna_provisioning.RegistrarAvailability) C.RustBuffer {
	return LowerIntoRustBuffer[*fauna_provisioning.RegistrarAvailability](c, value)
}

func (c FfiConverterOptionalRegistrarAvailability) LowerExternal(value *fauna_provisioning.RegistrarAvailability) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*fauna_provisioning.RegistrarAvailability](c, value))
}

func (_ FfiConverterOptionalRegistrarAvailability) Write(writer io.Writer, value *fauna_provisioning.RegistrarAvailability) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		fauna_provisioning.FfiConverterRegistrarAvailabilityINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalRegistrarAvailability struct{}

func (_ FfiDestroyerOptionalRegistrarAvailability) Destroy(value *fauna_provisioning.RegistrarAvailability) {
	if value != nil {
		fauna_provisioning.FfiDestroyerRegistrarAvailability{}.Destroy(*value)
	}
}

type FfiConverterOptionalUpdateChannel struct{}

var FfiConverterOptionalUpdateChannelINSTANCE = FfiConverterOptionalUpdateChannel{}

func (c FfiConverterOptionalUpdateChannel) Lift(rb RustBufferI) *fauna_provisioning.UpdateChannel {
	return LiftFromRustBuffer[*fauna_provisioning.UpdateChannel](c, rb)
}

func (_ FfiConverterOptionalUpdateChannel) Read(reader io.Reader) *fauna_provisioning.UpdateChannel {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := fauna_provisioning.FfiConverterUpdateChannelINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalUpdateChannel) Lower(value *fauna_provisioning.UpdateChannel) C.RustBuffer {
	return LowerIntoRustBuffer[*fauna_provisioning.UpdateChannel](c, value)
}

func (c FfiConverterOptionalUpdateChannel) LowerExternal(value *fauna_provisioning.UpdateChannel) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*fauna_provisioning.UpdateChannel](c, value))
}

func (_ FfiConverterOptionalUpdateChannel) Write(writer io.Writer, value *fauna_provisioning.UpdateChannel) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		fauna_provisioning.FfiConverterUpdateChannelINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalUpdateChannel struct{}

func (_ FfiDestroyerOptionalUpdateChannel) Destroy(value *fauna_provisioning.UpdateChannel) {
	if value != nil {
		fauna_provisioning.FfiDestroyerUpdateChannel{}.Destroy(*value)
	}
}

type FfiConverterOptionalSecretString struct{}

var FfiConverterOptionalSecretStringINSTANCE = FfiConverterOptionalSecretString{}

func (c FfiConverterOptionalSecretString) Lift(rb RustBufferI) *fauna_core.SecretString {
	return LiftFromRustBuffer[*fauna_core.SecretString](c, rb)
}

func (_ FfiConverterOptionalSecretString) Read(reader io.Reader) *fauna_core.SecretString {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := fauna_core.FfiConverterTypeSecretStringINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalSecretString) Lower(value *fauna_core.SecretString) C.RustBuffer {
	return LowerIntoRustBuffer[*fauna_core.SecretString](c, value)
}

func (c FfiConverterOptionalSecretString) LowerExternal(value *fauna_core.SecretString) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*fauna_core.SecretString](c, value))
}

func (_ FfiConverterOptionalSecretString) Write(writer io.Writer, value *fauna_core.SecretString) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		fauna_core.FfiConverterTypeSecretStringINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalSecretString struct{}

func (_ FfiDestroyerOptionalSecretString) Destroy(value *fauna_core.SecretString) {
	if value != nil {
		fauna_core.FfiDestroyerTypeSecretString{}.Destroy(*value)
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

type FfiConverterSequenceBillOfMaterialsItem struct{}

var FfiConverterSequenceBillOfMaterialsItemINSTANCE = FfiConverterSequenceBillOfMaterialsItem{}

func (c FfiConverterSequenceBillOfMaterialsItem) Lift(rb RustBufferI) []BillOfMaterialsItem {
	return LiftFromRustBuffer[[]BillOfMaterialsItem](c, rb)
}

func (c FfiConverterSequenceBillOfMaterialsItem) Read(reader io.Reader) []BillOfMaterialsItem {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]BillOfMaterialsItem, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterBillOfMaterialsItemINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceBillOfMaterialsItem) Lower(value []BillOfMaterialsItem) C.RustBuffer {
	return LowerIntoRustBuffer[[]BillOfMaterialsItem](c, value)
}

func (c FfiConverterSequenceBillOfMaterialsItem) LowerExternal(value []BillOfMaterialsItem) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]BillOfMaterialsItem](c, value))
}

func (c FfiConverterSequenceBillOfMaterialsItem) Write(writer io.Writer, value []BillOfMaterialsItem) {
	if len(value) > math.MaxInt32 {
		panic("[]BillOfMaterialsItem is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterBillOfMaterialsItemINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceBillOfMaterialsItem struct{}

func (FfiDestroyerSequenceBillOfMaterialsItem) Destroy(sequence []BillOfMaterialsItem) {
	for _, value := range sequence {
		FfiDestroyerBillOfMaterialsItem{}.Destroy(value)
	}
}

type FfiConverterSequenceDnsPlanLine struct{}

var FfiConverterSequenceDnsPlanLineINSTANCE = FfiConverterSequenceDnsPlanLine{}

func (c FfiConverterSequenceDnsPlanLine) Lift(rb RustBufferI) []DnsPlanLine {
	return LiftFromRustBuffer[[]DnsPlanLine](c, rb)
}

func (c FfiConverterSequenceDnsPlanLine) Read(reader io.Reader) []DnsPlanLine {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]DnsPlanLine, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterDnsPlanLineINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceDnsPlanLine) Lower(value []DnsPlanLine) C.RustBuffer {
	return LowerIntoRustBuffer[[]DnsPlanLine](c, value)
}

func (c FfiConverterSequenceDnsPlanLine) LowerExternal(value []DnsPlanLine) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]DnsPlanLine](c, value))
}

func (c FfiConverterSequenceDnsPlanLine) Write(writer io.Writer, value []DnsPlanLine) {
	if len(value) > math.MaxInt32 {
		panic("[]DnsPlanLine is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterDnsPlanLineINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceDnsPlanLine struct{}

func (FfiDestroyerSequenceDnsPlanLine) Destroy(sequence []DnsPlanLine) {
	for _, value := range sequence {
		FfiDestroyerDnsPlanLine{}.Destroy(value)
	}
}

type FfiConverterSequenceDnsRecordPlain struct{}

var FfiConverterSequenceDnsRecordPlainINSTANCE = FfiConverterSequenceDnsRecordPlain{}

func (c FfiConverterSequenceDnsRecordPlain) Lift(rb RustBufferI) []DnsRecordPlain {
	return LiftFromRustBuffer[[]DnsRecordPlain](c, rb)
}

func (c FfiConverterSequenceDnsRecordPlain) Read(reader io.Reader) []DnsRecordPlain {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]DnsRecordPlain, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterDnsRecordPlainINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceDnsRecordPlain) Lower(value []DnsRecordPlain) C.RustBuffer {
	return LowerIntoRustBuffer[[]DnsRecordPlain](c, value)
}

func (c FfiConverterSequenceDnsRecordPlain) LowerExternal(value []DnsRecordPlain) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]DnsRecordPlain](c, value))
}

func (c FfiConverterSequenceDnsRecordPlain) Write(writer io.Writer, value []DnsRecordPlain) {
	if len(value) > math.MaxInt32 {
		panic("[]DnsRecordPlain is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterDnsRecordPlainINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceDnsRecordPlain struct{}

func (FfiDestroyerSequenceDnsRecordPlain) Destroy(sequence []DnsRecordPlain) {
	for _, value := range sequence {
		FfiDestroyerDnsRecordPlain{}.Destroy(value)
	}
}

type FfiConverterSequenceFieldMetaPlain struct{}

var FfiConverterSequenceFieldMetaPlainINSTANCE = FfiConverterSequenceFieldMetaPlain{}

func (c FfiConverterSequenceFieldMetaPlain) Lift(rb RustBufferI) []FieldMetaPlain {
	return LiftFromRustBuffer[[]FieldMetaPlain](c, rb)
}

func (c FfiConverterSequenceFieldMetaPlain) Read(reader io.Reader) []FieldMetaPlain {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]FieldMetaPlain, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterFieldMetaPlainINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceFieldMetaPlain) Lower(value []FieldMetaPlain) C.RustBuffer {
	return LowerIntoRustBuffer[[]FieldMetaPlain](c, value)
}

func (c FfiConverterSequenceFieldMetaPlain) LowerExternal(value []FieldMetaPlain) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]FieldMetaPlain](c, value))
}

func (c FfiConverterSequenceFieldMetaPlain) Write(writer io.Writer, value []FieldMetaPlain) {
	if len(value) > math.MaxInt32 {
		panic("[]FieldMetaPlain is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterFieldMetaPlainINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceFieldMetaPlain struct{}

func (FfiDestroyerSequenceFieldMetaPlain) Destroy(sequence []FieldMetaPlain) {
	for _, value := range sequence {
		FfiDestroyerFieldMetaPlain{}.Destroy(value)
	}
}

type FfiConverterSequenceLeftoverLine struct{}

var FfiConverterSequenceLeftoverLineINSTANCE = FfiConverterSequenceLeftoverLine{}

func (c FfiConverterSequenceLeftoverLine) Lift(rb RustBufferI) []LeftoverLine {
	return LiftFromRustBuffer[[]LeftoverLine](c, rb)
}

func (c FfiConverterSequenceLeftoverLine) Read(reader io.Reader) []LeftoverLine {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]LeftoverLine, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterLeftoverLineINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceLeftoverLine) Lower(value []LeftoverLine) C.RustBuffer {
	return LowerIntoRustBuffer[[]LeftoverLine](c, value)
}

func (c FfiConverterSequenceLeftoverLine) LowerExternal(value []LeftoverLine) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]LeftoverLine](c, value))
}

func (c FfiConverterSequenceLeftoverLine) Write(writer io.Writer, value []LeftoverLine) {
	if len(value) > math.MaxInt32 {
		panic("[]LeftoverLine is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterLeftoverLineINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceLeftoverLine struct{}

func (FfiDestroyerSequenceLeftoverLine) Destroy(sequence []LeftoverLine) {
	for _, value := range sequence {
		FfiDestroyerLeftoverLine{}.Destroy(value)
	}
}

type FfiConverterSequenceManagedServerRow struct{}

var FfiConverterSequenceManagedServerRowINSTANCE = FfiConverterSequenceManagedServerRow{}

func (c FfiConverterSequenceManagedServerRow) Lift(rb RustBufferI) []ManagedServerRow {
	return LiftFromRustBuffer[[]ManagedServerRow](c, rb)
}

func (c FfiConverterSequenceManagedServerRow) Read(reader io.Reader) []ManagedServerRow {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ManagedServerRow, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterManagedServerRowINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceManagedServerRow) Lower(value []ManagedServerRow) C.RustBuffer {
	return LowerIntoRustBuffer[[]ManagedServerRow](c, value)
}

func (c FfiConverterSequenceManagedServerRow) LowerExternal(value []ManagedServerRow) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ManagedServerRow](c, value))
}

func (c FfiConverterSequenceManagedServerRow) Write(writer io.Writer, value []ManagedServerRow) {
	if len(value) > math.MaxInt32 {
		panic("[]ManagedServerRow is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterManagedServerRowINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceManagedServerRow struct{}

func (FfiDestroyerSequenceManagedServerRow) Destroy(sequence []ManagedServerRow) {
	for _, value := range sequence {
		FfiDestroyerManagedServerRow{}.Destroy(value)
	}
}

type FfiConverterSequenceRestoredPredecessorSeed struct{}

var FfiConverterSequenceRestoredPredecessorSeedINSTANCE = FfiConverterSequenceRestoredPredecessorSeed{}

func (c FfiConverterSequenceRestoredPredecessorSeed) Lift(rb RustBufferI) []RestoredPredecessorSeed {
	return LiftFromRustBuffer[[]RestoredPredecessorSeed](c, rb)
}

func (c FfiConverterSequenceRestoredPredecessorSeed) Read(reader io.Reader) []RestoredPredecessorSeed {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]RestoredPredecessorSeed, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterRestoredPredecessorSeedINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceRestoredPredecessorSeed) Lower(value []RestoredPredecessorSeed) C.RustBuffer {
	return LowerIntoRustBuffer[[]RestoredPredecessorSeed](c, value)
}

func (c FfiConverterSequenceRestoredPredecessorSeed) LowerExternal(value []RestoredPredecessorSeed) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]RestoredPredecessorSeed](c, value))
}

func (c FfiConverterSequenceRestoredPredecessorSeed) Write(writer io.Writer, value []RestoredPredecessorSeed) {
	if len(value) > math.MaxInt32 {
		panic("[]RestoredPredecessorSeed is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterRestoredPredecessorSeedINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceRestoredPredecessorSeed struct{}

func (FfiDestroyerSequenceRestoredPredecessorSeed) Destroy(sequence []RestoredPredecessorSeed) {
	for _, value := range sequence {
		FfiDestroyerRestoredPredecessorSeed{}.Destroy(value)
	}
}

type FfiConverterSequenceRetireStepRow struct{}

var FfiConverterSequenceRetireStepRowINSTANCE = FfiConverterSequenceRetireStepRow{}

func (c FfiConverterSequenceRetireStepRow) Lift(rb RustBufferI) []RetireStepRow {
	return LiftFromRustBuffer[[]RetireStepRow](c, rb)
}

func (c FfiConverterSequenceRetireStepRow) Read(reader io.Reader) []RetireStepRow {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]RetireStepRow, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterRetireStepRowINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceRetireStepRow) Lower(value []RetireStepRow) C.RustBuffer {
	return LowerIntoRustBuffer[[]RetireStepRow](c, value)
}

func (c FfiConverterSequenceRetireStepRow) LowerExternal(value []RetireStepRow) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]RetireStepRow](c, value))
}

func (c FfiConverterSequenceRetireStepRow) Write(writer io.Writer, value []RetireStepRow) {
	if len(value) > math.MaxInt32 {
		panic("[]RetireStepRow is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterRetireStepRowINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceRetireStepRow struct{}

func (FfiDestroyerSequenceRetireStepRow) Destroy(sequence []RetireStepRow) {
	for _, value := range sequence {
		FfiDestroyerRetireStepRow{}.Destroy(value)
	}
}

type FfiConverterSequenceDnsZone struct{}

var FfiConverterSequenceDnsZoneINSTANCE = FfiConverterSequenceDnsZone{}

func (c FfiConverterSequenceDnsZone) Lift(rb RustBufferI) []fauna_provisioning.DnsZone {
	return LiftFromRustBuffer[[]fauna_provisioning.DnsZone](c, rb)
}

func (c FfiConverterSequenceDnsZone) Read(reader io.Reader) []fauna_provisioning.DnsZone {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]fauna_provisioning.DnsZone, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, fauna_provisioning.FfiConverterDnsZoneINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceDnsZone) Lower(value []fauna_provisioning.DnsZone) C.RustBuffer {
	return LowerIntoRustBuffer[[]fauna_provisioning.DnsZone](c, value)
}

func (c FfiConverterSequenceDnsZone) LowerExternal(value []fauna_provisioning.DnsZone) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]fauna_provisioning.DnsZone](c, value))
}

func (c FfiConverterSequenceDnsZone) Write(writer io.Writer, value []fauna_provisioning.DnsZone) {
	if len(value) > math.MaxInt32 {
		panic("[]fauna_provisioning.DnsZone is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		fauna_provisioning.FfiConverterDnsZoneINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceDnsZone struct{}

func (FfiDestroyerSequenceDnsZone) Destroy(sequence []fauna_provisioning.DnsZone) {
	for _, value := range sequence {
		fauna_provisioning.FfiDestroyerDnsZone{}.Destroy(value)
	}
}

type FfiConverterSequenceServerTypeInfo struct{}

var FfiConverterSequenceServerTypeInfoINSTANCE = FfiConverterSequenceServerTypeInfo{}

func (c FfiConverterSequenceServerTypeInfo) Lift(rb RustBufferI) []fauna_provisioning.ServerTypeInfo {
	return LiftFromRustBuffer[[]fauna_provisioning.ServerTypeInfo](c, rb)
}

func (c FfiConverterSequenceServerTypeInfo) Read(reader io.Reader) []fauna_provisioning.ServerTypeInfo {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]fauna_provisioning.ServerTypeInfo, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, fauna_provisioning.FfiConverterServerTypeInfoINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceServerTypeInfo) Lower(value []fauna_provisioning.ServerTypeInfo) C.RustBuffer {
	return LowerIntoRustBuffer[[]fauna_provisioning.ServerTypeInfo](c, value)
}

func (c FfiConverterSequenceServerTypeInfo) LowerExternal(value []fauna_provisioning.ServerTypeInfo) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]fauna_provisioning.ServerTypeInfo](c, value))
}

func (c FfiConverterSequenceServerTypeInfo) Write(writer io.Writer, value []fauna_provisioning.ServerTypeInfo) {
	if len(value) > math.MaxInt32 {
		panic("[]fauna_provisioning.ServerTypeInfo is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		fauna_provisioning.FfiConverterServerTypeInfoINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceServerTypeInfo struct{}

func (FfiDestroyerSequenceServerTypeInfo) Destroy(sequence []fauna_provisioning.ServerTypeInfo) {
	for _, value := range sequence {
		fauna_provisioning.FfiDestroyerServerTypeInfo{}.Destroy(value)
	}
}

type FfiConverterSequenceVpsLocation struct{}

var FfiConverterSequenceVpsLocationINSTANCE = FfiConverterSequenceVpsLocation{}

func (c FfiConverterSequenceVpsLocation) Lift(rb RustBufferI) []fauna_provisioning.VpsLocation {
	return LiftFromRustBuffer[[]fauna_provisioning.VpsLocation](c, rb)
}

func (c FfiConverterSequenceVpsLocation) Read(reader io.Reader) []fauna_provisioning.VpsLocation {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]fauna_provisioning.VpsLocation, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, fauna_provisioning.FfiConverterVpsLocationINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceVpsLocation) Lower(value []fauna_provisioning.VpsLocation) C.RustBuffer {
	return LowerIntoRustBuffer[[]fauna_provisioning.VpsLocation](c, value)
}

func (c FfiConverterSequenceVpsLocation) LowerExternal(value []fauna_provisioning.VpsLocation) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]fauna_provisioning.VpsLocation](c, value))
}

func (c FfiConverterSequenceVpsLocation) Write(writer io.Writer, value []fauna_provisioning.VpsLocation) {
	if len(value) > math.MaxInt32 {
		panic("[]fauna_provisioning.VpsLocation is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		fauna_provisioning.FfiConverterVpsLocationINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceVpsLocation struct{}

func (FfiDestroyerSequenceVpsLocation) Destroy(sequence []fauna_provisioning.VpsLocation) {
	for _, value := range sequence {
		fauna_provisioning.FfiDestroyerVpsLocation{}.Destroy(value)
	}
}

type FfiConverterSequenceCapabilityPlain struct{}

var FfiConverterSequenceCapabilityPlainINSTANCE = FfiConverterSequenceCapabilityPlain{}

func (c FfiConverterSequenceCapabilityPlain) Lift(rb RustBufferI) []CapabilityPlain {
	return LiftFromRustBuffer[[]CapabilityPlain](c, rb)
}

func (c FfiConverterSequenceCapabilityPlain) Read(reader io.Reader) []CapabilityPlain {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]CapabilityPlain, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterCapabilityPlainINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceCapabilityPlain) Lower(value []CapabilityPlain) C.RustBuffer {
	return LowerIntoRustBuffer[[]CapabilityPlain](c, value)
}

func (c FfiConverterSequenceCapabilityPlain) LowerExternal(value []CapabilityPlain) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]CapabilityPlain](c, value))
}

func (c FfiConverterSequenceCapabilityPlain) Write(writer io.Writer, value []CapabilityPlain) {
	if len(value) > math.MaxInt32 {
		panic("[]CapabilityPlain is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterCapabilityPlainINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceCapabilityPlain struct{}

func (FfiDestroyerSequenceCapabilityPlain) Destroy(sequence []CapabilityPlain) {
	for _, value := range sequence {
		FfiDestroyerCapabilityPlain{}.Destroy(value)
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

type FfiConverterMapStringHostedAuthState struct{}

var FfiConverterMapStringHostedAuthStateINSTANCE = FfiConverterMapStringHostedAuthState{}

func (c FfiConverterMapStringHostedAuthState) Lift(rb RustBufferI) map[string]HostedAuthState {
	return LiftFromRustBuffer[map[string]HostedAuthState](c, rb)
}

func (_ FfiConverterMapStringHostedAuthState) Read(reader io.Reader) map[string]HostedAuthState {
	result := make(map[string]HostedAuthState)
	length := readInt32(reader)
	for i := int32(0); i < length; i++ {
		key := FfiConverterStringINSTANCE.Read(reader)
		value := FfiConverterHostedAuthStateINSTANCE.Read(reader)
		result[key] = value
	}
	return result
}

func (c FfiConverterMapStringHostedAuthState) Lower(value map[string]HostedAuthState) C.RustBuffer {
	return LowerIntoRustBuffer[map[string]HostedAuthState](c, value)
}

func (c FfiConverterMapStringHostedAuthState) LowerExternal(value map[string]HostedAuthState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[map[string]HostedAuthState](c, value))
}

func (_ FfiConverterMapStringHostedAuthState) Write(writer io.Writer, mapValue map[string]HostedAuthState) {
	if len(mapValue) > math.MaxInt32 {
		panic("map[string]HostedAuthState is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(mapValue)))
	for key, value := range mapValue {
		FfiConverterStringINSTANCE.Write(writer, key)
		FfiConverterHostedAuthStateINSTANCE.Write(writer, value)
	}
}

type FfiDestroyerMapStringHostedAuthState struct{}

func (_ FfiDestroyerMapStringHostedAuthState) Destroy(mapValue map[string]HostedAuthState) {
	for key, value := range mapValue {
		FfiDestroyerString{}.Destroy(key)
		FfiDestroyerHostedAuthState{}.Destroy(value)
	}
}

type FfiConverterMapStringSecretString struct{}

var FfiConverterMapStringSecretStringINSTANCE = FfiConverterMapStringSecretString{}

func (c FfiConverterMapStringSecretString) Lift(rb RustBufferI) map[string]fauna_core.SecretString {
	return LiftFromRustBuffer[map[string]fauna_core.SecretString](c, rb)
}

func (_ FfiConverterMapStringSecretString) Read(reader io.Reader) map[string]fauna_core.SecretString {
	result := make(map[string]fauna_core.SecretString)
	length := readInt32(reader)
	for i := int32(0); i < length; i++ {
		key := FfiConverterStringINSTANCE.Read(reader)
		value := fauna_core.FfiConverterTypeSecretStringINSTANCE.Read(reader)
		result[key] = value
	}
	return result
}

func (c FfiConverterMapStringSecretString) Lower(value map[string]fauna_core.SecretString) C.RustBuffer {
	return LowerIntoRustBuffer[map[string]fauna_core.SecretString](c, value)
}

func (c FfiConverterMapStringSecretString) LowerExternal(value map[string]fauna_core.SecretString) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[map[string]fauna_core.SecretString](c, value))
}

func (_ FfiConverterMapStringSecretString) Write(writer io.Writer, mapValue map[string]fauna_core.SecretString) {
	if len(mapValue) > math.MaxInt32 {
		panic("map[string]fauna_core.SecretString is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(mapValue)))
	for key, value := range mapValue {
		FfiConverterStringINSTANCE.Write(writer, key)
		fauna_core.FfiConverterTypeSecretStringINSTANCE.Write(writer, value)
	}
}

type FfiDestroyerMapStringSecretString struct{}

func (_ FfiDestroyerMapStringSecretString) Destroy(mapValue map[string]fauna_core.SecretString) {
	for key, value := range mapValue {
		FfiDestroyerString{}.Destroy(key)
		fauna_core.FfiDestroyerTypeSecretString{}.Destroy(value)
	}
}

const (
	uniffiRustFuturePollReady      int8 = 0
	uniffiRustFuturePollMaybeReady int8 = 1
)

type rustFuturePollFunc func(C.uint64_t, C.UniffiRustFutureContinuationCallback, C.uint64_t)
type rustFutureCompleteFunc[T any] func(C.uint64_t, *C.RustCallStatus) T
type rustFutureFreeFunc func(C.uint64_t)

//export fauna_onboarding_machine_uniffiFutureContinuationCallback
func fauna_onboarding_machine_uniffiFutureContinuationCallback(data C.uint64_t, pollResult C.int8_t) {
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
			(C.UniffiRustFutureContinuationCallback)(C.fauna_onboarding_machine_uniffiFutureContinuationCallback),
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

//export fauna_onboarding_machine_uniffiFreeGorutine
func fauna_onboarding_machine_uniffiFreeGorutine(data C.uint64_t) {
	handle := cgo.Handle(uintptr(data))
	defer handle.Delete()

	guard := handle.Value().(chan struct{})
	guard <- struct{}{}
}

// Render a price in cents as a localized-feeling string. Currency is the
// ISO-4217 code (e.g. "USD"). The output is `"{whole}.{cents:02} {ccy}"` —
// callers that want full localization should ignore this and pass the raw
// (cents, currency) through their platform's number formatter.
func FormatPrice(cents uint64, currency string) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_func_format_price(FfiConverterUint64INSTANCE.Lower(cents), FfiConverterStringINSTANCE.Lower(currency), _uniffiStatus),
		}
	}))
}

// The top-level domain (TLD) of a wizard `handle`'s mail domain — e.g.
// `alice@example.xyz` → `Some("xyz")`. Drives the onboarding "no supported
// registrar carries .{tld}" message (`dns-no-provider-message`,
// `docs/goal/behavior/onboarding.md` § dns_config :176): the client shows it
// with `.{tld}` only when there is a real TLD, so all seven apps agree on
// what counts as one instead of each re-deriving it.
//
// Returns `None` (— i.e. "no TLD, hide the `.{tld}` message") when the handle
// has no `@`, an empty local part or empty domain (`@x` / `x@`), or a domain
// with no `.` (still being typed, or a bare hostname). The domain extraction
// mirrors `machine::parse_handle_domain`; the TLD is the substring after the
// domain's last `.`. This is the canonical shape: web + iOS already matched it,
// and macOS (previously returned the whole domain on a dot-less domain) is now
// migrated onto this helper via the UniFFI export below. The web (already
// correct — uniformity) and android (`substringAfterLast('.')` on the whole
// handle leaks a local-part dot — bug-fix) legs are the cross-area remainder.
func HandleTld(handle string) *string {
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_func_handle_tld(FfiConverterStringINSTANCE.Lower(handle), _uniffiStatus),
		}
	}))
}

// Re-qualify a bare-localpart admin `handle` with its mail domain for a
// **factory-reset re-claim** (`mail-bridge-lifecycle.md` § Factory reset →
// *re-claim handle sourcing*). The nest stores **bare** localparts but
// auto-registers the primary mail domain from the handle's `@domain` at claim,
// so the re-onboard after a wipe must carry `localpart@domain` — otherwise the
// re-claimed nest comes back with no primary mail domain and the bridge idles
// (mail silently breaks) on any box the `ensure_primary_mail_domain` safety net
// doesn't cover (notably a custom-domain admin whose handle domain ≠ the nest's
// `handle_domain`).
//
// A `handle` that is empty or already contains `@` is returned unchanged.
// Otherwise the domain is the supplied `domain` (the cached mail domain, if
// non-empty), else the host parsed from `nest_url`; if neither yields a host
// the bare localpart is returned (the safety net then covers the common case).
// The caller decides where the bare handle comes from — linux sources it
// authoritatively from the live admin session before the wipe (`fauna.account.get`)
// with a cache fallback, others from the cache — but the qualification itself
// (and the nest-URL-host parse) lives here once for all seven apps rather than
// being re-derived per client.
func QualifyReclaimHandle(handle string, domain *string, nestUrl string) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_func_qualify_reclaim_handle(FfiConverterStringINSTANCE.Lower(handle), FfiConverterOptionalStringINSTANCE.Lower(domain), FfiConverterStringINSTANCE.Lower(nestUrl), _uniffiStatus),
		}
	}))
}

// RAM gate for the `vps-config-mail-mode-toggle`: whether `st` may be selected
// given the chosen mail mode. When mail is ON every plan must have
// `mem_gb ≥ 2.0` (the scanner sidecars need the RAM); when mail is OFF the
// social-only box runs lean, so every plan — including the 1 GB tier — is
// allowed. Clients filter/disable the `vps-server-type-radio` through this
// single predicate so the gate stays uniform across all six (priority #2). Per
// `docs/goal/behavior/onboarding.md` §5 (RAM gate).
func ServerTypeAllowedForMail(st fauna_provisioning.ServerTypeInfo, enableMail bool) bool {
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_onboarding_machine_fn_func_server_type_allowed_for_mail(
			CFromRustBuffer(fauna_provisioning.FfiConverterServerTypeInfoINSTANCE.LowerExternal(st)), FfiConverterBoolINSTANCE.Lower(enableMail), _uniffiStatus)
	}))
}

// Display label for a server-type radio button:
// "{id} — {vcpu} vCPU / {mem_gb} GB / {disk_gb} GB disk / {price/100} {ccy}/mo".
func ServerTypeLabel(st fauna_provisioning.ServerTypeInfo) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_func_server_type_label(
				CFromRustBuffer(fauna_provisioning.FfiConverterServerTypeInfoINSTANCE.LowerExternal(st)), _uniffiStatus),
		}
	}))
}

// [`RecoveryEntryOutcome::message`] as a free function — the UniFFI door the
// FFI apps (android, apple, windows) render the restore's answer through, so
// they read the same table tui, linux and web do instead of re-deriving it.
func RecoveryEntryOutcomeMessage(outcome RecoveryEntryOutcome) *fauna_core.LocalizedText {
	return FfiConverterOptionalLocalizedTextINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_onboarding_machine_fn_func_recovery_entry_outcome_message(FfiConverterRecoveryEntryOutcomeINSTANCE.Lower(outcome), _uniffiStatus),
		}
	}))
}

// `AWAITING_DNS_POLL_MS` for the native apps — see
// [`invite_recheck_poll_ms`].
func AwaitingDnsPollMs() uint64 {
	return FfiConverterUint64INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_fauna_onboarding_machine_fn_func_awaiting_dns_poll_ms(_uniffiStatus)
	}))
}

// `INVITE_RECHECK_POLL_MS` for the native apps — UniFFI exports functions, not
// constants, so the value still crosses the FFI from this one definition.
func InviteRecheckPollMs() uint64 {
	return FfiConverterUint64INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_fauna_onboarding_machine_fn_func_invite_recheck_poll_ms(_uniffiStatus)
	}))
}
