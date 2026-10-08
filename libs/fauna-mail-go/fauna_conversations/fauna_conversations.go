package fauna_conversations

// #include <fauna_conversations.h>
import "C"

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_client_moderation"
	"github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"
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
		C.ffi_fauna_conversations_rustbuffer_free(cb.inner, status)
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
		return C.ffi_fauna_conversations_rustbuffer_from_bytes(foreign, status)
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

	FfiConverterSnapshotObserverINSTANCE.register()
	uniffiCheckChecksums()
}

func uniffiCheckChecksums() {
	// Get the bindings contract version from our ComponentInterface
	bindingsContractVersion := 30
	// Get the scaffolding contract version by calling the into the dylib
	scaffoldingContractVersion := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint32_t {
		return C.ffi_fauna_conversations_uniffi_contract_version()
	})
	if bindingsContractVersion != int(scaffoldingContractVersion) {
		// If this happens try cleaning and rebuilding your project
		panic("fauna_conversations: UniFFI contract version mismatch")
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_rail_as_str()
		})
		if checksum != 18458 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_rail_as_str: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_rail_glyph()
		})
		if checksum != 37156 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_rail_glyph: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_rail_parse()
		})
		if checksum != 37485 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_rail_parse: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_try_parse_typed_address()
		})
		if checksum != 64056 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_try_parse_typed_address: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_recipient_resolve_status()
		})
		if checksum != 17934 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_recipient_resolve_status: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_more_grid_emojis()
		})
		if checksum != 17214 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_more_grid_emojis: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_quickset_emojis()
		})
		if checksum != 57306 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_quickset_emojis: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_class_attr_token()
		})
		if checksum != 4102 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_class_attr_token: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_class_label()
		})
		if checksum != 45598 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_class_label: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_history_policy_editor_choices()
		})
		if checksum != 10327 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_history_policy_editor_choices: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_history_policy_label()
		})
		if checksum != 5530 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_history_policy_label: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_history_policy_token()
		})
		if checksum != 55067 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_history_policy_token: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_invitation_text()
		})
		if checksum != 59442 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_invitation_text: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_join_rule_editor_choices()
		})
		if checksum != 48758 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_join_rule_editor_choices: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_join_rule_label()
		})
		if checksum != 24823 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_join_rule_label: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_join_rule_token()
		})
		if checksum != 57677 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_join_rule_token: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_member_chip_text()
		})
		if checksum != 47107 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_member_chip_text: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_notice_attr_token()
		})
		if checksum != 34772 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_notice_attr_token: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_notice_for()
		})
		if checksum != 23452 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_notice_for: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_notice_label()
		})
		if checksum != 14409 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_notice_label: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_pending_invite_text()
		})
		if checksum != 23257 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_pending_invite_text: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_prospective_class()
		})
		if checksum != 19135 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_prospective_class: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_role_attr_token()
		})
		if checksum != 42875 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_role_attr_token: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_may_name_labeler_kind()
		})
		if checksum != 48734 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_may_name_labeler_kind: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_settings_admin_at()
		})
		if checksum != 51795 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_settings_admin_at: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_settings_edits()
		})
		if checksum != 1719 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_settings_edits: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_settings_is_eligible()
		})
		if checksum != 59635 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_settings_is_eligible: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_settings_is_owner_at()
		})
		if checksum != 30569 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_settings_is_owner_at: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_settings_labeler_staged()
		})
		if checksum != 26326 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_settings_labeler_staged: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_settings_labeler_toggle_live()
		})
		if checksum != 54770 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_settings_labeler_toggle_live: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_settings_seed()
		})
		if checksum != 44899 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_settings_seed: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_settings_set_history_policy()
		})
		if checksum != 12265 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_settings_set_history_policy: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_settings_set_join_rule()
		})
		if checksum != 36395 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_settings_set_join_rule: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_settings_toggle_admin()
		})
		if checksum != 31831 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_settings_toggle_admin: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_settings_toggle_labeler()
		})
		if checksum != 49597 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_settings_toggle_labeler: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_settings_toggle_nest_read()
		})
		if checksum != 53522 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_settings_toggle_nest_read: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_settings_toggle_transfer()
		})
		if checksum != 38031 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_settings_toggle_transfer: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_room_settings_transfer_staged_at()
		})
		if checksum != 59716 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_room_settings_transfer_staged_at: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_func_next_sort_order()
		})
		if checksum != 19490 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_func_next_sort_order: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_accept_add_participant_chip()
		})
		if checksum != 28076 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_accept_add_participant_chip: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_accept_current_recipient_chip()
		})
		if checksum != 50637 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_accept_current_recipient_chip: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_accept_new_thread_chip()
		})
		if checksum != 2385 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_accept_new_thread_chip: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_accept_room_invitation()
		})
		if checksum != 40067 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_accept_room_invitation: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_add_attachment()
		})
		if checksum != 5061 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_add_attachment: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_add_new_thread_attachment()
		})
		if checksum != 9407 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_add_new_thread_attachment: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_add_observer()
		})
		if checksum != 22594 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_add_observer: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_add_participant()
		})
		if checksum != 8167 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_add_participant: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_add_participant_inner()
		})
		if checksum != 60466 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_add_participant_inner: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_add_reply_recipient()
		})
		if checksum != 48279 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_add_reply_recipient: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_apply_room_settings()
		})
		if checksum != 8451 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_apply_room_settings: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_appoint_admin()
		})
		if checksum != 52200 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_appoint_admin: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_attachment_bytes()
		})
		if checksum != 3147 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_attachment_bytes: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_attachment_resident()
		})
		if checksum != 11285 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_attachment_resident: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_cache_attachment_bytes()
		})
		if checksum != 37360 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_cache_attachment_bytes: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_cancel_add_participant()
		})
		if checksum != 13512 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_cancel_add_participant: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_cancel_new_conversation()
		})
		if checksum != 50718 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_cancel_new_conversation: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_channel_hex()
		})
		if checksum != 28656 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_channel_hex: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_clear_for_identity_change()
		})
		if checksum != 24553 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_clear_for_identity_change: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_clear_observers()
		})
		if checksum != 46116 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_clear_observers: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_clear_page_error()
		})
		if checksum != 10313 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_clear_page_error: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_clear_selection()
		})
		if checksum != 4259 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_clear_selection: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_confirm_add_participant()
		})
		if checksum != 28485 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_confirm_add_participant: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_conversation_sort_json()
		})
		if checksum != 9198 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_conversation_sort_json: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_conversation_threads_json()
		})
		if checksum != 25516 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_conversation_threads_json: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_deactivate_new_conversation()
		})
		if checksum != 41563 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_deactivate_new_conversation: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_decline_room_invitation()
		})
		if checksum != 51263 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_decline_room_invitation: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_delete_message()
		})
		if checksum != 13170 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_delete_message: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_demote_admin()
		})
		if checksum != 5800 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_demote_admin: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_drafts_snapshot_bytes()
		})
		if checksum != 46350 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_drafts_snapshot_bytes: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_engine_served_elsewhere()
		})
		if checksum != 60414 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_engine_served_elsewhere: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_ensure_keypackages()
		})
		if checksum != 57595 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_ensure_keypackages: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_ensure_last_resort_keypackage()
		})
		if checksum != 50259 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_ensure_last_resort_keypackage: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_evict_person_everywhere()
		})
		if checksum != 24894 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_evict_person_everywhere: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_fauna_roster_actors()
		})
		if checksum != 43253 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_fauna_roster_actors: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_handle_for_person()
		})
		if checksum != 35460 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_handle_for_person: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_identity_epoch()
		})
		if checksum != 61473 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_identity_epoch: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_ingest_inbound()
		})
		if checksum != 50589 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_ingest_inbound: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_leave_room()
		})
		if checksum != 3068 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_leave_room: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_mark_read()
		})
		if checksum != 27822 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_mark_read: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_observer_count()
		})
		if checksum != 63369 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_observer_count: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_open_add_participant()
		})
		if checksum != 57164 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_open_add_participant: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_page_error_diagnostic()
		})
		if checksum != 50878 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_page_error_diagnostic: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_receive_stopped()
		})
		if checksum != 12012 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_receive_stopped: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_remove_attachment()
		})
		if checksum != 17150 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_remove_attachment: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_remove_new_thread_attachment()
		})
		if checksum != 29057 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_remove_new_thread_attachment: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_remove_participant()
		})
		if checksum != 51815 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_remove_participant: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_remove_reply_recipient()
		})
		if checksum != 1294 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_remove_reply_recipient: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_rename_thread()
		})
		if checksum != 56183 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_rename_thread: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_reply_preview()
		})
		if checksum != 46249 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_reply_preview: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_resolve_link_preview()
		})
		if checksum != 63724 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_resolve_link_preview: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_resolve_recipient()
		})
		if checksum != 56023 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_resolve_recipient: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_restore_drafts()
		})
		if checksum != 50869 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_restore_drafts: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_restore_drafts_at()
		})
		if checksum != 25268 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_restore_drafts_at: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_retire_conversations_engine()
		})
		if checksum != 53254 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_retire_conversations_engine: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_reveal_remote_images()
		})
		if checksum != 39634 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_reveal_remote_images: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_seat_address_for()
		})
		if checksum != 56807 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_seat_address_for: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_secure_channel_count()
		})
		if checksum != 56508 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_secure_channel_count: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_select_thread()
		})
		if checksum != 15733 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_select_thread: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_select_thread_and_message()
		})
		if checksum != 51280 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_select_thread_and_message: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_send()
		})
		if checksum != 22878 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_send: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_send_new_thread()
		})
		if checksum != 25132 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_send_new_thread: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_add_participant_recipient_input()
		})
		if checksum != 19482 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_add_participant_recipient_input: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_compose_body()
		})
		if checksum != 18962 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_compose_body: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_compose_subject()
		})
		if checksum != 19483 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_compose_subject: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_engine_served_elsewhere()
		})
		if checksum != 12105 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_engine_served_elsewhere: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_new_thread_body()
		})
		if checksum != 30949 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_new_thread_body: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_new_thread_home_nest()
		})
		if checksum != 54374 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_new_thread_home_nest: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_new_thread_recipient_input()
		})
		if checksum != 50891 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_new_thread_recipient_input: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_new_thread_subject()
		})
		if checksum != 36363 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_new_thread_subject: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_page_error()
		})
		if checksum != 37162 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_page_error: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_reply_to()
		})
		if checksum != 13622 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_reply_to: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_room_history_policy()
		})
		if checksum != 52460 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_room_history_policy: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_room_join_rule()
		})
		if checksum != 29503 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_room_join_rule: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_room_labelers()
		})
		if checksum != 56896 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_room_labelers: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_room_nest_read()
		})
		if checksum != 30199 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_room_nest_read: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_search_query()
		})
		if checksum != 52214 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_search_query: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_set_sort()
		})
		if checksum != 32061 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_set_sort: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_snapshot()
		})
		if checksum != 8631 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_snapshot: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_start_new_conversation()
		})
		if checksum != 31243 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_start_new_conversation: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_start_reply()
		})
		if checksum != 35186 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_start_reply: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_thread_detail()
		})
		if checksum != 52718 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_thread_detail: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_toggle_reaction()
		})
		if checksum != 64311 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_toggle_reaction: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_toggle_topic()
		})
		if checksum != 2767 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_toggle_topic: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_transfer_room_ownership()
		})
		if checksum != 43727 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_transfer_room_ownership: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_unopenable_mail_count()
		})
		if checksum != 36028 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_unopenable_mail_count: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_unread_total()
		})
		if checksum != 34031 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_unread_total: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationsmanager_withdraw_room_invite()
		})
		if checksum != 47195 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationsmanager_withdraw_room_invite: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_snapshotobserver_on_changed()
		})
		if checksum != 45847 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_snapshotobserver_on_changed: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationssession_conv_receive_cycles_json()
		})
		if checksum != 36547 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationssession_conv_receive_cycles_json: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationssession_conv_receive_now()
		})
		if checksum != 22495 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationssession_conv_receive_now: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationssession_conversation_threads_json()
		})
		if checksum != 25696 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationssession_conversation_threads_json: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationssession_ingest_welcome()
		})
		if checksum != 41470 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationssession_ingest_welcome: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationssession_manager()
		})
		if checksum != 33919 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationssession_manager: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationssession_mls_folded_commits_json()
		})
		if checksum != 42529 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationssession_mls_folded_commits_json: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationssession_moderation_local_detections()
		})
		if checksum != 16917 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationssession_moderation_local_detections: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationssession_moderation_message_body()
		})
		if checksum != 28624 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationssession_moderation_message_body: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationssession_moderation_remove_local_detection()
		})
		if checksum != 11036 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationssession_moderation_remove_local_detection: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationssession_poll_conversations()
		})
		if checksum != 47649 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationssession_poll_conversations: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationssession_poll_mail()
		})
		if checksum != 25467 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationssession_poll_mail: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationssession_set_self_address()
		})
		if checksum != 53940 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationssession_set_self_address: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_method_conversationssession_start_receive_loop()
		})
		if checksum != 4479 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_method_conversationssession_start_receive_loop: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_conversations_checksum_constructor_conversationsmanager_new()
		})
		if checksum != 38381 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_conversations: uniffi_fauna_conversations_checksum_constructor_conversationsmanager_new: UniFFI API checksum mismatch")
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

type ConversationsManagerInterface interface {
	// Commit `addr` as a chip on the add-participant picker (clears the
	// raw input). No-op if the overlay isn't open. Mirrors
	// `accept_new_thread_chip`.
	AcceptAddParticipantChip(addr TypedAddress)
	// Accept the current text on whichever recipient picker is active as a
	// chip — the add-participant overlay's picker takes priority over the
	// new-thread picker. Returns `true` iff a chip was pushed. Single entry
	// point for the "press Enter" / "click suggestion" path on every app (and
	// the e2e test command).
	//
	// **Commits only a probed address.** The chip is the picker's `resolved`
	// address — what the async [`Self::resolve_recipient`] confirmed, carrying
	// the real rail/identity — and nothing else: no shape parse of the raw
	// text from `Resolving` / `Error` / `NotFound` / `Idle`. Enter before the
	// probe lands, or after it errored, commits nothing (the status line says
	// why); an app whose Enter path wants to be robust resolves first and then
	// accepts (web, linux, tui's agent path). The former shape fallback was one
	// of the four points at which a Fauna peer that did not answer became a
	// silently-accepted email chip (`docs/goal/ui/conversations.md` § Errors &
	// edge cases → *The picker tells the truth*, 2026-08-29).
	AcceptCurrentRecipientChip() bool
	AcceptNewThreadChip(addr TypedAddress)
	// `room-invitation-accept-button[i]` — accept the standing invitation
	// `id` names: the account is seated on the room's floor, and the room
	// opens as a thread and is selected. It reads nothing until a member with
	// key authority keys it in, which happens on that member's device
	// (`FaunaMlsBackend::tend_community_room`); the thread fills from then.
	//
	// Returns the room's thread, or `None` when the invitation is no longer
	// standing or the accept was refused — the refusal on the page's
	// `error-message`.
	AcceptRoomInvitation(id int64) *ThreadId
	// Stage `bytes` as an attachment on `id`'s compose draft. Returns the
	// `blob_hash` the bytes were cached under (the handle the client renders /
	// the wire message references). Capability gating (`supports_attachments`)
	// is the client's responsibility — a rail that drops attachments just won't
	// inline them on `send`.
	AddAttachment(id ThreadId, filename string, mimeType string, bytes []byte) string
	// Stage an attachment on the new-thread compose (`attachment-button` before
	// a thread exists). Mirrors [`Self::add_attachment`] for the single-slot
	// new-thread draft; `send_new_thread` carries the staged attachments onto
	// the materialized thread's draft. `None` if no new-thread compose is open.
	AddNewThreadAttachment(filename string, mimeType string, bytes []byte) *string
	AddObserver(obs SnapshotObserver)
	// Add a participant to a thread.
	//
	// For `(FaunaMls, OneToOne)` this forks a new MLS group thread (the
	// original 1:1 stays intact) — an MLS-protocol necessity: a pairwise
	// MLS group can't gain a member, so the only path to a 3-person
	// encrypted thread is a fresh group. Every other `(rail, flavor)` adds
	// the participant in place: for SMTP this is just a wider CC, and for a
	// bridge whose vector declares it, the far network's own add; there is
	// no cryptographic fork. (A bridge declaring none has
	// `supports_membership_change == false`, so this is unreachable for
	// it.) Returns the (possibly new) thread id.
	AddParticipant(id ThreadId, addr TypedAddress) *ThreadId
	AddParticipantInner(id ThreadId, addr TypedAddress) *ThreadId
	// Add `addr` to the editable reply To line (`dm-reply-recipient-add`),
	// de-duplicated by address identity. No-op if already present.
	AddReplyRecipient(id ThreadId, addr TypedAddress)
	// `room-settings-save-button` — commit every change the editor staged,
	// each as its own policy commit, in [`RoomSettingsDraft::edits`]' order
	// (the two rules, then the appointments and demotions, then the
	// hand-over last). **Stops at the first refusal**, which the calls above
	// have already painted on the page's `error-message`, and returns
	// whether all of them landed — the editor closes only on `true`
	// (`ui/conversations.md` § Element IDs, the `room_settings` sub-page).
	//
	// The whole loop lives here rather than in each app's Save handler so
	// the seven agree on the order and on what "landed" means (priority #2);
	// an app stages through [`RoomSettingsDraft`] and calls this once.
	//
	// [`RoomSettingsDraft`]: crate::room_settings::RoomSettingsDraft
	// [`RoomSettingsDraft::edits`]: crate::room_settings::RoomSettingsDraft::edits
	ApplyRoomSettings(id ThreadId, edits []RoomSettingsEdit) bool
	// Appoint a member of a governed room an admin (owner only,
	// `conversation-rooms.md` § Roles and authorization).
	AppointAdmin(id ThreadId, addr TypedAddress)
	// The shared attachment loader: the plaintext bytes cached under
	// `blob_hash`, if any (populated by `add_attachment`, the send echo, and the
	// inbound parse). The per-app render path resolves a
	// `dm-attachment-image[i]` / `-file[i]` to real bytes through this. `None`
	// for an unknown, not-yet-fetched, or **evicted** hash — the store is a
	// bounded cache (`store::attachments`), and every app already paints the
	// declared filename + size when the bytes are absent. A miss on a handle
	// whose coordinates are remembered (a FaunaMls blob this device received or sent, a mail
	// record, or either restored from a `history/<ch>` slice) marks it wanted
	// and pokes the receive loop, whose next cycle fetches it again on that
	// rail's sweep and notifies observers, so the same render asks again and
	// hits.
	AttachmentBytes(blobHash string) *[]byte
	// Whether `blob_hash`'s bytes are resident in this device's attachment store
	// right now — a read-only peek that neither copies the bytes nor marks a miss
	// wanted. An app that reuses a bubble across renders keys its rebuild on this
	// as well as on the message: evicting bytes or fetching them again changes no
	// message, so a bubble keyed on the message alone keeps painting what it
	// painted before (`conversations.md` § Attachments → *Retention*).
	AttachmentResident(blobHash string) bool
	// Cache an inbound attachment's plaintext bytes under its `blob_hash`. The
	// receive path ([`crate::backends::smtp::ingest_inbound_record`], and the
	// wasm JS-driven poll's equivalent) calls this for each extracted attachment
	// before ingesting the message, so the rendered bubble resolves the handle
	// to real bytes via [`Self::attachment_bytes`].
	//
	// **The key is verified, not trusted.** `blob_hash` must be the lowercase-hex
	// BLAKE3 of `bytes` — the invariant `conversations.md` § Attachments states —
	// and a pair that does not satisfy it is **refused with a warn**, exactly as
	// an undecryptable attachment is skipped. Silent rather than fallible on
	// purpose: this method is `uniffi::export`ed, so a `Result` would change the
	// FFI signature for all 7 apps to report a condition none of them can act on.
	//
	// Verifying here rather than at each caller is what makes the guarantee
	// hold: this is the ONLY door into the store, it is a public FFI surface, and
	// the store is a single map shared across every channel and room. Before
	// this, `blob_hash` on the FaunaMls path was **sender-authored** (it rides
	// inside the sealed `ChannelAttachment`) and unchecked, so any co-member who
	// knew a hash could overwrite those bytes for every conversation at once —
	// and, because `send` re-resolves bytes from this same map by hash
	// (`resolve_attachments`), could swap a victim's *staged outgoing* file
	// between staging and send, leaving them to seal and sign it under their own
	// filename. Both arms close here: substituting bytes under a fixed key now
	// requires a BLAKE3 preimage.
	//
	// Idempotent for a truthful pair — re-caching bytes under their own hash is
	// a harmless overwrite (it is now genuinely the same value).
	CacheAttachmentBytes(blobHash string, bytes []byte)
	// Close the add-participant overlay, discarding any in-progress input.
	CancelAddParticipant()
	CancelNewConversation()
	// The hex nest-channel id a FaunaMls thread is bound to (once its MLS group
	// has bootstrapped / a Welcome materialized it), or `None` for an unbound
	// thread. Surfaced into the e2e state protocol so a real-wire test can
	// observe the channel an MLS conversation carries on the nest.
	ChannelHex(id ThreadId) *string
	// Wipe every piece of identity-scoped state this manager holds, preserving
	// registered backends and observers. Call it on the **production** account
	// switch / sign-out path, before the incoming identity's session activates:
	// the manager is an identity-scoped singleton that outlives the shell
	// teardown, so a switch that skipped this would render the outgoing
	// account's threads, drafts and selection to the incoming one.
	//
	// Deliberately NOT `_for_test`-gated. It is the production twin of the
	// harness's [`Self::clear_for_test`] (which delegates here), and it exists
	// because the two callers are genuinely different: the e2e harness wipes
	// *between tests*, apple's `tearDownSessionForSwitch` and linux's
	// identity-switch path wipe *between identities*. Apple reached the seam
	// for the production job while every apple FFI recipe still shipped
	// `test-helpers`, which `testing.md` § convention 15's recipe split removes
	// — so the production path needs a production method, not a wider gate.
	// Named to match the identity-change helpers linux already has
	// (`critical_alerts::clear_for_identity_change`, `screen_lock::…`).
	//
	// It also advances [`Self::identity_epoch`], which retires every launch
	// restore the outgoing account started ([`Self::restore_drafts_at`]).
	ClearForIdentityChange()
	// Drop all registered observers. Each GTK client attaches a fresh observer
	// per authenticated-window build (`views/conversations` → `observer::attach`),
	// and nothing prunes the old ones — so across a sign-out→re-auth cycle they
	// accumulate unboundedly, and (because the linux authenticated window's
	// widget tree does not finalize on `destroy()`) the *stale* panes their
	// receiver loops hold stay live in the a11y tree, re-presenting dialogs and
	// fielding clicks against a now-shut-down runtime. Call this at sign-out
	// (before the window is rebuilt): dropping the observers closes each stale
	// loop's channel, so its `rx.recv()` returns `Err` and the loop breaks,
	// releasing its panes. The next window build re-attaches a live observer.
	ClearObservers()
	// Clear a previous gesture's error. Every producer calls this on entry: a
	// new gesture supersedes the last one's outcome, so a success — or merely
	// a retry in flight — never leaves the page accusing the user of a failure
	// they have already moved past. A no-op (and **no** emit) when nothing is
	// set, so the common path costs no observer tick.
	ClearPageError()
	ClearSelection()
	// Confirm the add-participant overlay (`thread-add-participant-button` →
	// confirm). Snapshot: a `(FaunaMls, OneToOne)` add forks a fresh
	// participant-keyed group thread (Signal semantics) and selects it; every
	// other `(rail, flavor)` adds in place. Wire op: an in-place add on an
	// already-bound FaunaMls group posts the MLS Commit + Welcome via the
	// backend; a 1:1 fork bootstraps its group lazily on first `send`
	// (Track B), and non-FaunaMls rails have no wire membership op. Returns
	// the resulting thread id, or `None` if the overlay wasn't open / no
	// address was picked.
	ConfirmAddParticipant() *ThreadId
	// The `data.conversation_sort` e2e state value, JSON-encoded — the list's
	// active order in `setSort`'s serde spelling, from the same
	// [`crate::state_json::conversation_sort_json`] tui and linux call in
	// process, so no app spells the order names itself.
	ConversationSortJson() string
	// The `data.conversation_threads` e2e state rows, JSON-encoded — the
	// manager-level twin of [`crate::session::ConversationsSession::conversation_threads_json`],
	// for a caller that has a manager but no (or not yet activated) session:
	// apple's e2e login path deliberately never activates a real
	// `ConversationsSession` (`ConversationsVM.activate` — kept unset so the
	// deterministic `test-helpers` mock backends stay in effect), so an app
	// reading only off `session?.conversationThreadsJson()` sees an
	// empty list forever regardless of what `inject_inbound_for_test` staged
	// (a refactor regression — every apple conversations e2e test
	// broke the same way, on both macOS and iOS, the moment it switched off
	// the manager-backed row builder). Delegates to the same
	// [`crate::state_json::conversation_threads_json`] free function, so the
	// row shape (including `participant_actor_ids`) is identical either way.
	ConversationThreadsJson() string
	DeactivateNewConversation()
	// `room-invitation-decline-button[i]` — the invitation stops standing for
	// this account. The room is not told: a refusal the inviter could read
	// would give declining an audience (`conversation-rooms.md` § Join rules
	// and invites).
	DeclineRoomInvitation(id int64)
	// Delete `message` in `thread` (`dm-message-delete-button` —
	// conversations.md § Reactions & message delete). FaunaMls-only, sender-
	// only: no-op if the thread lacks `supports_message_delete`, or if the
	// message is not owned by the local user. Optimistically tombstones the
	// message in the manager-owned deleted set (re-emitting immediately), then
	// fires the backend wire op best-effort — a wire failure is warned, not
	// propagated; the optimistic state stands. The client renders the deleted
	// placeholder off `MessageSnapshot.deleted`; body/attachments are left
	// intact in the snapshot (the CLIENT decides the render, not the store).
	DeleteMessage(thread ThreadId, message MessageId)
	// Demote an admin of a governed room to member (owner only).
	DemoteAdmin(id ThreadId, addr TypedAddress)
	// The whole conversations-rail draft set serialised to its canonical
	// at-rest bytes. The client glue seals these under the owner's `BackupKey`
	// and uploads them to the `__drafts` reserved folder after a compose
	// change (`docs/goal/behavior/file-sync.md` § Drafts Sync). Byte-stable for
	// equal logical state, so an unchanged draft set re-uploads identically.
	DraftsSnapshotBytes() []byte
	// See [`Self::set_engine_served_elsewhere`]. Read by each app's
	// `error-message` projection with top precedence — a standing condition
	// that must never be masked by an unrelated gesture's `page_error`.
	EngineServedElsewhere() bool
	// Top up the local actor's one-time key-package pool on the nest to
	// `target`. Thin FFI-facing wrapper over the FaunaMls rail backend's
	// [`RailBackend::ensure_keypackages`](crate::backend::RailBackend::ensure_keypackages)
	// (the backend owns the `MlsEngine` that mints packages — the manager stays
	// free of MLS crypto per `conversations.md` § Architectural rules #2).
	// Login-time replenish is session-owned
	// ([`ConversationsSession::start_receive_loop`](crate::ConversationsSession::start_receive_loop)
	// runs it **after** the MLS state-replica restore — a restore swaps the
	// engine's provider storage, so a package minted before it would lose its
	// private init key; `devices.md` § Cross-device MLS group-state sync);
	// settings pages drive manual refresh through the same surface. Returns the
	// number uploaded (`0` if at/above target, or if no FaunaMls backend is
	// registered on this client). `target` of `0` is a count-only no-op.
	//
	// A mint writes fresh private init keys into the engine's provider storage
	// only — this notifies the observers so the debounced replica autosave
	// persists them (an unsaved mint leaves peers holding key packages whose
	// init keys exist nowhere durable).
	EnsureKeypackages(target uint64) (uint64, error)
	// Publish the actor's mandatory **last-resort** key package if absent. Thin
	// FFI-facing wrapper over the FaunaMls rail backend's
	// [`RailBackend::ensure_last_resort_keypackage`](crate::backend::RailBackend::ensure_last_resort_keypackage).
	// Idempotent (the nest keeps a single last-resort row per actor), so every
	// login calls it (session-owned, after the replica restore — see
	// [`Self::ensure_keypackages`]) to stay `addressable` after the one-time
	// pool drains (`docs/goal/architecture/federation.md` § Key packages).
	// No-op if no FaunaMls backend is registered on this client. Notifies the
	// observers on success — the backend mints a fresh package each call, and
	// its private init key must reach the replica autosave like any other mint.
	EnsureLastResortKeypackage() error
	// Remove `person` from **every** group of the owner's they are currently
	// in — the *Remove* of the two review surfaces that render a person
	// outside any one group (`identity-succession.md` § Propagation → *MLS
	// groups*). See [`crate::eviction`] for why the operation lives here, and
	// [`CrossGroupEviction::earned_verdict`] for what its outcome may and may
	// not be recorded as.
	//
	// **Which groups: the ones the person is in *now*, re-derived — and
	// re-derived from the authority, not from the chat snapshot.** The review
	// item stores `(person, raising event, reason)` and deliberately not the
	// groups, so this enumerates live membership. That is right for *removing*
	// and would be wrong for *raising* — a raise is a fact about membership
	// across the compromise window, and re-deriving it would flag people who
	// joined afterwards (§ Propagation). Two operations, two different
	// questions, one of which must never borrow the other's answer. *Which*
	// live membership is the second half of the same rule, and it is the
	// security-bearing half — see the contract note below.
	//
	// **A 1:1's honest peer is left alone**, and the gate is the thread
	// *flavor* deliberately rather than the `supports_membership_change`
	// capability the obvious draft reaches for. That capability cannot
	// discriminate here: a `TypedAddress::Fauna` participant only ever appears
	// on a FaunaMls thread, and **every** FaunaMls flavor is
	// membership-capable, so the check is unreachable beside the actor-id
	// match below — a guard that can never fire, and therefore one no test can
	// ever pin (found by mutating it away and watching all three pins stay
	// green). The reachable distinction is the one that matters anyway: a DM
	// is not a group, and "removing" the only other person from a 1:1 would
	// leave the owner alone in a thread the ordinary delete already handles —
	// a second removal mechanism of exactly the kind § Propagation refuses to
	// mint. What the flavor does **not** protect is a seat the 1:1 never
	// promised — see rule (5) below.
	// **⚠ Contract for every caller and every future edit: this driver's roster
	// source must be the same source the flag it acts on was raised from.**
	// That flag is `SweepReport::unattested_members`, read off the MLS **engine**
	// (`MlsEngine::group_members`), so membership here is asked of the rail
	// ([`crate::backend::RailBackend::authoritative_roster`]) and NOT of
	// `ThreadDetail::participants`. The snapshot is a local view that no inbound
	// Commit reconciles: where the two disagree — a foreign-authored add being
	// the adversarial case — a snapshot-sourced enumeration `continue`s past the
	// group, records no failure, and so earns `Removed` while the person is
	// still seated, which is rule (3)'s harm reached through the enumeration
	// door. A surface built on this must
	// likewise never render *who remains* from the snapshot.
	//
	// **The span — § Propagation rule (5), ratified 2026-08-10.** The raise's
	// span is every MLS group the engine holds; *Remove*'s reach is decided
	// per channel class, and a raised seat it cannot clear rides
	// [`CrossGroupEviction::unreachable`] as a typed fact — blocking the
	// verdict, never escaping silently. Per class: a chat-**group** thread is
	// evicted directly; a 1:1's one honest peer (the snapshot-listed
	// participant) is never touched (rule (2)) but **any engine seat beyond
	// the snapshot-listed peer is evicted like a group seat** — the chat poll
	// applies membership Commits with no flavor gate, so a thief's Commit can
	// seat a third identity in a DM channel; a chat channel with no bound
	// thread here blocks as
	// [`crate::eviction::UnreachableSeatClass::ChatGroupNoThreadHere`]; a
	// folder channel blocks as
	// [`crate::eviction::UnreachableSeatClass::FolderChannel`]
	// (its removal is the folder plane's, and a left set converges the same
	// way); a scheduling channel never blocks — no Commit is ever applied to
	// one, so nothing can be planted there and nothing needs removing. The
	// unbound enumeration follows the same source rule as the roster:
	// [`crate::backend::RailBackend::unbound_seats_of`], off the engine.
	EvictPersonEverywhere(person ActorId) CrossGroupEviction
	// Every Fauna identity on any of this member's thread rosters — each once,
	// in thread-store order.
	//
	// The **rosters**, deliberately not [`Self::snapshot`]: that is the
	// conversations page's view and applies the page's search filter, so a
	// consumer walking it sees only the threads the search box currently
	// shows. The peer-anchor harvest sweep is the caller this exists for —
	// who it harvests must not depend on what the owner last typed into a
	// filter, least of all since a succession statement can wait on that
	// harvest (`identity-succession.md` § The succession statement → *the
	// harvest wait*).
	FaunaRosterActors() []ActorId
	// The handle `person` is currently seated under in the owner's own
	// threads, for the two review surfaces that render a person **outside any
	// one group** and therefore have no chip to read a name off.
	//
	// ⚠ **Display only, and never an identity claim.** The whole review exists
	// because an identity a thief seated in a group may wear any handle it
	// likes, so this is what to *show* the owner beside the row, never what to
	// match on — every membership decision here keys on the actor id
	// ([`crate::eviction::is_person`]).
	//
	// **`None` is an ordinary answer, not a failure**, and the permanent view
	// is where it happens: that surface holds a backlog someone postponed, and
	// a flagged person may have left every group since. A caller renders the
	// row regardless — dropping it would hide an item nobody could then close,
	// which is the failure this surface exists to prevent.
	//
	// Re-derived rather than stored, for [`Self::evict_person_everywhere`]'s
	// reason: the review item deliberately keeps no group list, and a handle
	// cached at raise time would go stale exactly when it matters (a rename
	// between the sweep and the review).
	// ⚠ **An empty display is `None`, not `Some("")`, and the skip happens
	// DURING the search** (2026-08-10). A roster built from a Welcome carries
	// no handles at all — `ingest_welcome` writes `handle: String::new()` for
	// every member, because a leaf credential has none to give — so the
	// seated-but-nameless row is the *ordinary* case for any group the owner
	// joined rather than created, not a corner. Returning `Some("")` walked
	// straight past every caller's no-name fallback (tui's
	// `REVIEW_UNKNOWN_PERSON`) and rendered a review row with a blank where the
	// person goes; the `None` arm was already written and already handled, and
	// this is what makes it reachable. Filtering *inside* the scan rather than
	// on its result is the load-bearing half: one person is commonly seated in
	// several threads, and a nameless Welcome row would otherwise end the
	// search and shadow a handle-bearing row one thread over.
	//
	// **Only a proven seat lends its handle** (`contacts.md` § The private
	// overlay → *The paint gate*): a row is
	// consulted only in a thread whose bound group holds `person` as a
	// verified leaf ([`Self::proven_roster`]). A participant row's `(handle,
	// actor id)` pair comes from a resolve answer the dial rule leaves unbound
	// on the actor-id side, so a nest at the dialed domain that answers with
	// this person's id would otherwise put ITS handle on the person's
	// genuinely nameless seat one thread over — the same borrowed-identity
	// harm as the nickname, reached through the backfill. A thread the rail
	// keeps no roster for (not yet bootstrapped) lends nothing; once the group
	// seats the person's own key, the handle the dialed domain routes to that
	// key is theirs to lend.
	HandleForPerson(person ActorId) *string
	// The identity epoch: advanced by every [`Self::clear_for_identity_change`].
	// A shell reads it **before** starting a detached read of account-scoped
	// state (the launch `__drafts` fetch) and hands it back with the result
	// ([`Self::restore_drafts_at`]), which refuses a result the outgoing
	// account started. Opaque: only equality with a later read means anything.
	IdentityEpoch() uint64
	// Ingest one decrypted inbound message: bucket it via the rail backend,
	// resolve its thread (by participants / subject / reply-reference), and
	// append it.
	//
	// **Message-ID dedup.** A message whose id is already held in the store is
	// skipped (no second append). This is what keeps a server-side **Sent** copy
	// from double-showing in-session: a first-party `fauna.email.send` is echoed
	// locally on send (`Self::send`, keyed by the RFC `Message-ID` the client
	// minted), and the SAME message then arrives back over `fauna.email.sent.fetch`
	// as nest's durable Sent copy (`smtp-server.md` § Inbound client receive) — it
	// carries that identical `Message-ID`, so the dedup suppresses the duplicate
	// while leaving the message in the view. The caller's per-feed `seen` set
	// (`poll_inbound_mail`) keys on the server *segment-record* id and so can't
	// catch this (the local echo has no segment id); this id-level guard is the
	// only thing that can. After a client restart the in-memory store is empty, so
	// the Sent copy ingests normally and the sent message reloads — the durability
	// the server-side copy exists for. Idempotent across overlapping re-polls too.
	IngestInbound(msg RailInboundMessage) error
	// **Leave the room `id` is** — the roles table's *leave (remove self)*
	// row, and the gesture the departing-member report the nest has always
	// admitted was missing a producer for (`conversation-rooms.md`
	// § Roles and authorization → *Leaving — the mechanism*).
	//
	// **One verb, one door.** Every class leaves by the self-scoped
	// `room.leave`, which stamps this account's floor row and no one else's.
	// No app branches on class for this, and none should: the choice is the
	// mechanism's, not the gesture's.
	//
	// **The owner is refused here, before the wire.** Both nest doors enforce
	// "a room is never owner-less" on their own (one by rank, the other by
	// refusing an owner-less roster for a floor that names an owner), so this
	// check buys no safety — it buys the user a sentence that says what to do
	// instead of a wire refusal that says a room was malformed. The paint is gated on
	// `capabilities.can_leave_room` for the same reason, one layer up.
	//
	// **The thread stays.** This device keeps the generations and bubbles it
	// already holds: deleting them would destroy the user's own copy of a
	// conversation the user was legitimately part of, and would not un-read a
	// byte.
	//
	// ⚠ **The leaver's own room verbs do NOT close yet** — declared, not
	// overlooked (`conversation-rooms.md` § Implementation status today, the
	// departed-render gap). `my_role` is read from the room's signed policy,
	// which still names the leaver until a remaining owner or admin re-signs
	// it, so this device keeps rendering the rank it held. Closing it wants a
	// *departed* reading of the floor — this device has read the room's floor
	// and it does not name this account — which also covers being removed by
	// someone else, and is its own slice.
	LeaveRoom(id ThreadId)
	// Read thread `id` without opening it. Opening one needs no call: the
	// selection itself reads it ([`Self::notify`]), so an app that also calls
	// this on select is redundant, never wrong.
	MarkRead(id ThreadId)
	// Number of registered snapshot observers. Diagnostic-only — surfaced through
	// windows' e2e state protocol (`diagnostics.conversations_observer_count`) so a
	// per-manager observer-accumulation regression (a client constructing a fresh
	// observer on every re-navigation instead of reusing one for the manager's
	// lifetime) fails as itself in a headless assertion instead of as a downstream
	// dead-dispatch/retention symptom found later.
	ObserverCount() uint32
	// Open the add-participant overlay for `id`. No-op if the thread
	// doesn't exist. The picker starts empty (`Idle`).
	//
	// The thread lookup that answers "does it exist" also answers "does
	// confirming reach the wire"
	// ([`crate::capabilities::is_in_place_mls_group`]), so the overlay
	// carries its own offline-gate discriminant from the moment it opens and
	// no client has to re-derive it
	// ([`AddParticipantState::in_place_mls_group`]).
	OpenAddParticipant(id ThreadId)
	// The page error the last membership/label gesture reported, as a plain
	// `key: message` diagnostic — for a **test agent**, not for paint.
	//
	// [`Self::confirm_add_participant`], [`Self::remove_participant`] and
	// [`Self::rename_thread`] are UI gestures: they report a failed wire op by
	// stamping the page error and return `()` / `Option`, never a `Result`. An
	// agent command that awaits one of them therefore has **no return value to
	// check** — it must read this back, or it acks success for a wire op that
	// failed, which is the convention-11 swallow (`e2e-conventions.md` §
	// convention 11).
	//
	// `args["message"]` is what the *user* sees — the producers stamp it
	// through `BackendError::user_detail`, so a product statement (a nest
	// refusal, a version-mismatch sentence) reaches a test author verbatim,
	// while a **diagnostic** reads as the generic sentence and its detail is in
	// the log line beside the stamp. That is the send-slot taxonomy applying to
	// this element's other producer, not a gap: a diagnostic is by definition
	// not renderable, so a reader that needs the raw text needs the log.
	PageErrorDiagnostic() *string
	// Whether the receive loop currently serving this manager **died by
	// panic** — a standing condition each app's `error-message` projection
	// shows directly under [`Self::engine_served_elsewhere`], telling the user
	// to restart the app (`ui/conversations.md` § Errors & edge cases). Why a
	// restart and not a re-armed loop: `ReceiveLoopExit::Panicked`.
	//
	// Only the *current* loop counts: a newer loop started over this manager
	// clears it, and a superseded loop dying late never sets it. A designed
	// exit (the session dropped, the engine handed over) never sets it at all.
	// Lock-free, so a page render can never panic on a lock the dead pass
	// poisoned.
	ReceiveStopped() bool
	// Remove the staged attachment at `index` from `id`'s compose draft (the
	// `dm-attachment-*` remove affordance on the compose bar). The cached bytes
	// are left in the store — harmless (in-memory, cleared on next login) and
	// keeps the remove cheap; a shared blob could still be referenced elsewhere.
	RemoveAttachment(id ThreadId, index uint32)
	// Remove the staged attachment at `index` from the new-thread compose.
	RemoveNewThreadAttachment(index uint32)
	// Remove a participant (`thread-member-chip[i]` →
	// `manager.remove_participant`). Snapshot: drop the participant. Wire op:
	// a bound FaunaMls group posts an MLS Commit that re-keys the group so the
	// removed member can't follow forward (no Welcome). Non-FaunaMls rails are
	// snapshot-only.
	RemoveParticipant(id ThreadId, addr TypedAddress)
	// Remove `addr` from the editable reply To line
	// (`dm-reply-recipient-remove`). Drops the recipient from *this reply
	// only* — thread participants and history are untouched.
	RemoveReplyRecipient(id ThreadId, addr TypedAddress)
	// Rename a thread (`thread-rename-button` → `manager.rename_thread`).
	// Snapshot: relabel immediately. Wire op: a bound FaunaMls group posts an
	// encrypted `GroupMeta::NameChanged` application message so peers apply the
	// rename via `poll_inbound_conv` → [`Self::apply_inbound_rename`].
	RenameThread(id ThreadId, newLabel string)
	// What thread `id`'s compose bar previews for the reply in progress
	// (`dm-reply-preview`): the answered message's sender and a plain-text
	// excerpt, resolved against the thread's fetched messages. `None` when no
	// reply is armed — and when the answered message is not in the fetched
	// window, since an empty preview beats a stale or wrong one. One
	// derivation for every app: until 2026-09-21 tui and apple painted the
	// body, windows the sender's name, and web, android and linux the bare
	// message id.
	ReplyPreview(id ThreadId) *ReplyPreview
	// Resolve link-preview metadata (render-model.md § D4) for the bare `url` carried by a
	// `RenderBlock::LinkPreview { Resolving }` block in a loaded bubble — the conversations
	// twin of `FeedManager::resolve_link_preview`. Calls `fauna.linkpreview.resolve` once per
	// URL (the result is cached in [`resolved_previews`](Self::resolved_previews) keyed by
	// URL), maps the reply onto `PreviewState`, and notifies so the next
	// [`thread_detail`](Self::thread_detail) folds the matching block `Resolved`/`Failed`. A
	// transport error **or** an explicit `Failed` both collapse to the render model's terminal
	// `Failed` (the bubble falls back to the plain inline link, § D4). Idempotent: a repeat
	// call for an already-resolved URL is a no-op with NO further notify — render-loop-safe, so
	// a non-fire-once client observer re-calling from a notify-driven re-render gets a no-op. A
	// no-op too when no [`LinkPreviewRpc`] is wired (receive-only / SMTP-only).
	ResolveLinkPreview(url string)
	// Resolve the active recipient picker's current raw input across the
	// registered rail backends, then move its `resolve_state` Resolving →
	// Resolved / NotFound / Error and stash the resolved [`TypedAddress`] in
	// `RecipientPickerState::resolved`. A following
	// [`Self::accept_current_recipient_chip`] commits that *resolved* address
	// (e.g. a 64-hex actor id promoted to `Fauna` by the FaunaMls key-package
	// probe), not the format-only parse. The add-participant overlay's picker
	// takes priority over the new-thread picker, matching
	// `accept_current_recipient_chip`. No-op if neither picker is open or the
	// input is empty/whitespace.
	ResolveRecipient()
	// Restore the draft set from bytes the client glue fetched from `__drafts`
	// and unsealed with the owner's `BackupKey` — the load-on-launch /
	// cross-device catch-up path. Refreshes observers so the composer reflects
	// the restored drafts *before* the probe below runs. A corrupt or
	// unreadable blob is logged and ignored (start from empty drafts) rather
	// than failing the surface.
	//
	// The restore **fills**: it never clears a slot and never overwrites a
	// draft the user is composing into, however late the fetch lands
	// ([`DraftStore::restore_from_bytes`] owns the rule and the reasons).
	//
	// **Why this is `async` — a restore owes the recipient picker a probe.**
	// `conversations.md` § Errors & edge cases → *The picker tells the truth*
	// rule 1 ratifies that a non-empty recipient input is never `Idle`: typing
	// stamps `Resolving` and only [`Self::resolve_recipient`] moves it on. But
	// what *rests* is `raw_input` without the probe output — a running probe
	// cannot survive a relaunch, and resting `Resolving` would paint a spinner
	// nothing ever resolves (`store::drafts::persistable_picker` owns that
	// reasoning). So a restored picker arrives holding an address at `Idle`,
	// which was a **dead state**: nothing re-probes it, no chip can be
	// committed from it (`federation.md` § Peer-auth model → *Discovery-failure
	// semantics* — a chip is only ever a probed address), and the user cannot
	// even re-arm it by re-typing the same address, because every app shell
	// suppresses the echo of an unchanged field. Restoring the invariant in
	// each of the seven shells would be seven copies of the rule (priorities
	// #1/#2), so the restore issues the probe itself and every app gets it by
	// calling the one method it already calls.
	//
	// This is the restore at the **current** [`Self::identity_epoch`]. A shell
	// whose fetch can straddle an identity change on a manager it keeps across
	// the switch calls [`Self::restore_drafts_at`] with the epoch it captured
	// before the fetch instead.
	RestoreDrafts(bytes []byte)
	// [`Self::restore_drafts`] for a fetch the shell started at `epoch` (read
	// from [`Self::identity_epoch`] **before** the fetch). If
	// [`Self::clear_for_identity_change`] has run since, the bytes belong to
	// the outgoing account and the call does nothing: no fill, no notify, and
	// no recipient probe, so the incoming account's drafts are not shadowed,
	// its next autosave does not re-seal the outgoing account's text into its
	// own plane, and its rails are never asked to resolve the outgoing
	// account's recipient (`account-scoping.md` § The scoping taxonomy — the
	// writers of account-scoped state are retired by the same drop). The
	// refusal comes before the fill because the fill is what arms the probe.
	RestoreDraftsAt(epoch uint64, bytes []byte)
	// Retire the currently-registered conversations-engine rail
	// ([`Rail::FaunaMls`]) so a successor can take its place: drop the
	// registration and call [`RailBackend::retire`] on the way out, which
	// releases the MLS engine's conversations-engine role lock over
	// `mls_state.db`.
	//
	// **Why this is a separate method and not part of
	// [`Self::clear_for_identity_change`].** That one deliberately *preserves*
	// registered backends — its doc says so, and it is right to: it wipes the
	// identity-scoped *content* a switch must not carry over, while the rails
	// themselves are re-registered a moment later by the incoming session. This
	// method does the opposite and rarer thing, and the two run at different
	// moments: the wipe happens at the identity change, the retire happens
	// immediately before the successor engine is constructed. Folding them
	// together would leave a manager with no rails through every switch, and
	// would still miss the case this exists for — a **same-identity** re-login,
	// where nothing is identity-changing but a second engine over one
	// `mls_state.db` is refused all the same.
	//
	// Called by the shared native session factory
	// (`fauna_ffi::FfiNestClient::conversations_session*`) before
	// `MlsEngine::new`, so every UniFFI app — macOS, iOS, windows, android —
	// inherits the hand-over with no glue of its own. Idempotent, and a no-op
	// when no MLS rail is registered (a bare or mock-backed manager).
	//
	// The window between this call and the successor's `register_backend` is
	// deliberate and bounded: it is one engine construction wide, and during it
	// a send on the MLS rail resolves no backend and fails honestly, which is
	// the correct answer while the account's engine is mid-hand-over.
	//
	// **Also exported over UniFFI, for the shell that DROPS its manager**
	// (2026-09-02). "Every UniFFI app inherits the hand-over with no glue of
	// its own" was true only of shells that keep one manager across the
	// hand-over, because the factory can only retire the manager it is *handed*.
	// Windows replaces its process-wide manager at an actor change
	// (`ConversationsManagerHost.ResetForActorChange` — its sanctioned
	// exception to the no-swap rule, since the outgoing identity's rails,
	// observers and threads must not survive a switch), and it does so
	// **before** the successor build — so the factory's retire ran against a
	// brand-new manager with no MLS rail, took the documented no-op arm, and
	// the predecessor engine was left to a reference drop. That is the
	// mechanism this ruling refuses. A shell that drops its manager therefore
	// calls this on the OUTGOING one first; the call is the same explicit
	// ordered hand-over, moved to the one seam the factory cannot see.
	RetireConversationsEngine()
	// Opt this message into loading its remote images. Adds it to the in-memory
	// reveal set and re-emits, so the next [`thread_detail`](Self::thread_detail)
	// projects `RemoteImage.revealed: true` for it. **In-memory only** — the
	// no-persistence posture (html-mail.md § Rendering) is unchanged; a reveal
	// does not survive a restart. Idempotent: a repeat tap is a no-op insert
	// plus a harmless re-emit.
	RevealRemoteImages(messageId MessageId)
	// The address to seat for a roster actor id — the **one** resolution path
	// both roster seating sites use (`conversation-rooms.md` § Implementation
	// status today: "resolving a seated actor id to a handle — one follow-on
	// serving both this arm and the Welcome's").
	//
	// `backends::fauna_mls::ingest_welcome` (the members a Welcome brings) and
	// [`Self::apply_inbound_roster`] (a member another device added) both learn
	// their members from the MLS engine roster, which carries actor ids and
	// nothing else. Each used to seat `handle: String::new()` inline — so a
	// member arrived nameless even when this very device was already rendering
	// that same person, by name, one thread over.
	//
	// This resolves against that device-local knowledge
	// ([`Self::handle_for_person`]) and seats the handle when it finds one.
	// Three properties it is chosen for:
	//
	// - **Non-blocking, no I/O.** A pure in-memory scan of threads this device
	// already holds, so it is safe on [`Self::apply_inbound_roster`]'s path
	// — which runs inside the inbound poll's channel lock — and safe under
	// the e2e state provider's no-blocking-I/O rule (e2e convention 11's
	// corollary).
	// - **Honest when it finds nothing.** The seat keeps its empty handle and
	// [`TypedAddress::display`] renders the actor's short id, so an
	// unresolved member is a member with an elided name rather than a blank
	// row.
	// - **Never an identity claim.** A handle read off another thread is what
	// to *show*; every membership decision still keys on the actor id
	// ([`TypedAddress::same_participant`]) — [`Self::handle_for_person`]
	// carries the full argument.
	//
	// An actor this device has never met resolves to nothing today. The
	// remaining leg is a *network* id-keyed handle read: neither the floor
	// roster's member records
	// (`fauna_protocol::conversations::RoomRosterMemberWire`) nor
	// `fauna.profile.get` carries a handle to serve it yet.
	SeatAddressFor(actor ActorId) TypedAddress
	// The Status page's `status-mls-channels` count —
	// [`crate::snapshot::secure_channel_count`] over the live thread store,
	// under the same lock discipline as [`Self::unread_total`] (a read, never
	// inline on a mutator's thread). The shared snapshot's MLS leg
	// (`ui/status.md` § State & data shape) takes this number as its input.
	SecureChannelCount() uint64
	SelectThread(id ThreadId)
	// Select a thread **and** one message inside it — the whole of
	// `SearchNav::Mail`'s contract (`docs/goal/ui/search.md` § State & data
	// shape), and the only producer of a message selection.
	//
	// One write pair + one [`Self::notify`], so observers never see the
	// intermediate state where the thread has flipped but the message has not:
	// on an app that scrolls to the selection, that intermediate frame is a
	// visible jump to the wrong place.
	//
	// The `message_id` is **not** validated here. The thread's messages arrive
	// asynchronously, so a hit on a not-yet-fetched message would fail a
	// constructor-time check and lose a selection that becomes valid moments
	// later; [`Self::thread_detail`] resolves it against the live window on
	// every emit instead, which also makes a late arrival light up by itself.
	SelectThreadAndMessage(threadId ThreadId, messageId MessageId)
	// Send the per-thread compose draft for an existing thread, routing
	// through the thread's rail backend
	// (`docs/goal/ui/conversations.md` § User actions:
	// `dm-send-button → manager.send(thread_id)`). On success appends the
	// sent message and clears the draft; on failure stamps
	// `ComposeState.send_state = Failed { reason }` and returns the error.
	Send(id ThreadId) error
	// Materialize the new-thread compose into a real thread, then send it
	// (`dm-send-button → manager.send_new_thread()` for new-thread compose).
	// Returns the new thread id, or `None` when there is no active
	// new-thread compose / no committed recipient chip. On send failure the
	// thread is left materialized with a `Failed` draft (the user can retry).
	SendNewThread() (*ThreadId, error)
	// Update the add-participant picker's raw input. No-op if the overlay
	// isn't open. Same *typing owes a probe* rule as
	// [`Self::set_new_thread_recipient_input`].
	SetAddParticipantRecipientInput(text string)
	SetComposeBody(id ThreadId, body string)
	SetComposeSubject(id ThreadId, subject string)
	// Record whether this process is a non-holder of the conversations-engine
	// role (`MlsError::ServedElsewhere` at engine construction). Called by the
	// app's engine-construction site on every attempt — success clears it,
	// `ServedElsewhere` sets it, any other engine-init failure leaves it
	// cleared (a different, unrelated failure). A no-op (no `notify`) when
	// the value is unchanged, mirroring [`Self::clear_page_error`]'s
	// no-op-when-unset optimization.
	SetEngineServedElsewhere(served bool)
	SetNewThreadBody(body string)
	// `recipient-picker-home-nest-toggle` — whether the room about to be
	// created seats the user's home nest, which makes it a **community** room
	// the first send founds (`conversation-rooms.md` § The three classes).
	// The picker's class statement follows it
	// ([`crate::room::prospective_room_class`]). A no-op with no new-thread
	// compose open.
	SetNewThreadHomeNest(include bool)
	// Update the new-thread picker's raw input. **Typing owes a probe**: any
	// non-empty input parks the picker on `Resolving` until the async
	// [`Self::resolve_recipient`] reports; empty input is `Idle`. The state is
	// never derived from the text's *shape* — a shape-derived `Resolved` said
	// "Resolved" for a Fauna peer whose lookup had not started, and let Enter
	// commit an email chip for it (`docs/goal/ui/conversations.md` § Errors &
	// edge cases → *The picker tells the truth*, 2026-08-29).
	SetNewThreadRecipientInput(text string)
	SetNewThreadSubject(subject *string)
	// Stamp [`ConversationsSnapshot::error`] and emit, so the failure reaches
	// `error-message` on the very tick that produced it. Emitting here — at the
	// *event* — rather than leaving it for a caller's later `notify` is what
	// makes the surface hold for `remove_participant`/`rename_thread`, whose
	// own `notify` fires **before** their wire op runs.
	SetPageError(error fauna_core.LocalizedText)
	SetReplyTo(id ThreadId, msg *MessageId)
	// Change a governed room's history policy (`conversation-rooms.md`
	// § History for joiners) — the policy editor's second field. Owner or
	// admin.
	SetRoomHistoryPolicy(id ThreadId, policy HistoryPolicy)
	// Change a governed room's join rule (`conversation-rooms.md` § Join
	// rules and invites) — the policy editor's first field. Owner or admin;
	// a refusal surfaces on the page's `error-message` like every other
	// membership gesture.
	SetRoomJoinRule(id ThreadId, rule JoinRule)
	// Replace the transparent labelers a community room's home nest applies
	// to its messages — `labelers` are published labeler ids, lowercase hex
	// (`conversation-rooms.md` § The three classes → *What the home nest does
	// with its read*, purpose 2). Owner or admin; a refusal surfaces on the
	// page's `error-message` like every other policy gesture.
	SetRoomLabelers(id ThreadId, labelers []string)
	// Grant or withdraw the home nest's read of the community room `id` is —
	// the editor's `room-nest-read-toggle`, committed on Save through
	// [`Self::apply_room_settings`]. A key rotation, and so the owner's or an
	// admin's act; a refusal lands on the page's `error-message` like any
	// other room-settings failure.
	SetRoomNestRead(id ThreadId, reads bool)
	SetSearchQuery(query *string)
	SetSort(order SortOrder)
	Snapshot() ConversationsSnapshot
	StartNewConversation()
	// Seed a reply draft on `id` to `msg_id` (`dm-reply-button` /
	// `dm-reply-all-button`). Always sets `compose.reply_to`. On rails with
	// `supports_recipient_selection` (mail) it also seeds the editable To line
	// (`compose.reply_recipients`): `reply_all == false` → the replied
	// message's sender only; `reply_all == true` → every thread participant
	// except the local user (`backend.self_address()`). On other rails the To
	// line is hidden, so `reply_recipients` stays empty (recipients ARE the
	// thread membership). Either seed is then editable via
	// [`Self::add_reply_recipient`] / [`Self::remove_reply_recipient`]
	// (`conversations.md` § Participants vs reply recipients).
	StartReply(id ThreadId, msgId MessageId, replyAll bool)
	ThreadDetail(id ThreadId) *ThreadDetail
	// Toggle a reaction on `message` in `thread` (`dm-reaction-*` /
	// `dm-reaction-add` — conversations.md § Reactions & message delete).
	// FaunaMls-only: no-op if the thread has no `supports_reactions` capability
	// or the local actor is unknown. Optimistically pushes an Add or Remove
	// event onto the manager-owned reaction log (re-emitting so clients see the
	// update immediately), then fires the backend wire op best-effort — a wire
	// failure is warned, not propagated; the optimistic state stands.
	ToggleReaction(thread ThreadId, message MessageId, emoji string)
	ToggleTopic(id ThreadId)
	// Hand a governed room to another member (owner only,
	// `conversation-rooms.md` § Roles and authorization → *Ownership
	// transfer*). The owner's act posts the countersigned offer; the roles
	// flip on every seat once the new owner's device has committed it, so
	// the projection follows the agreed group context — not this call.
	TransferRoomOwnership(id ThreadId, addr TypedAddress)
	// How many received mail records this process skipped because they would
	// not open under the account's complete standing key set
	// ([`Self::note_unopenable_mail`]) — a standing truth each app's
	// `error-message` projection shows below every other page error
	// (`ui/conversations.md` § Errors & edge cases): the mailbox keeps
	// receiving past such a record, and the user is told that some mail did
	// not open on this device. `0` clears it. Not cleared by any gesture; a
	// record opening later (a re-drain after the account's keys changed)
	// retires its entry.
	UnopenableMailCount() uint32
	// The home-screen widget's number (`apps/common.md` § Home-screen
	// widget): [`crate::snapshot::sum_unread`] over **every** thread of the
	// account — the unfiltered store, not [`Self::snapshot`]'s list, which a
	// typed search narrows. A widget outside the app reports the account's
	// unread, and a transient filter inside the app must not move it. One
	// getter for every app's outside-the-app surface, so none keeps a tally of
	// its own or runs a second count query. Reads the thread store under its
	// own lock, so — like every snapshot read — an observer calls it after
	// the notifying mutation unwinds, never inline on the mutator's thread.
	UnreadTotal() uint32
	// Withdraw an invitation pending on the room `id` is — the gesture every
	// row of [`crate::room::RoomSnapshot::pending_invites`] carries
	// (`conversation-rooms.md` § Join rules and invites → *Pending invitations
	// are visible to whoever may withdraw them*). `invitee_actor_hex` is the
	// row's own `RoomPendingInviteSnapshot::invitee_actor_hex`.
	//
	// Acts at once, never staged through the editor's Save: a withdrawal is
	// not a policy edit. No app gates it — the home nest served the row to
	// this viewer *because* this viewer may withdraw it, and judges the act
	// again at its own door. The invitee is told nothing. On success the list
	// has been read again and the row is gone; a refusal lands on the page's
	// `error-message`.
	WithdrawRoomInvite(id ThreadId, inviteeActorHex string)
}
type ConversationsManager struct {
	ffiObject FfiObject
}

func NewConversationsManager() *ConversationsManager {
	return FfiConverterConversationsManagerINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_fauna_conversations_fn_constructor_conversationsmanager_new(_uniffiStatus)
	}))
}

// Commit `addr` as a chip on the add-participant picker (clears the
// raw input). No-op if the overlay isn't open. Mirrors
// `accept_new_thread_chip`.
func (_self *ConversationsManager) AcceptAddParticipantChip(addr TypedAddress) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_accept_add_participant_chip(
			_pointer, FfiConverterTypedAddressINSTANCE.Lower(addr), _uniffiStatus)
		return false
	})
}

// Accept the current text on whichever recipient picker is active as a
// chip — the add-participant overlay's picker takes priority over the
// new-thread picker. Returns `true` iff a chip was pushed. Single entry
// point for the "press Enter" / "click suggestion" path on every app (and
// the e2e test command).
//
// **Commits only a probed address.** The chip is the picker's `resolved`
// address — what the async [`Self::resolve_recipient`] confirmed, carrying
// the real rail/identity — and nothing else: no shape parse of the raw
// text from `Resolving` / `Error` / `NotFound` / `Idle`. Enter before the
// probe lands, or after it errored, commits nothing (the status line says
// why); an app whose Enter path wants to be robust resolves first and then
// accepts (web, linux, tui's agent path). The former shape fallback was one
// of the four points at which a Fauna peer that did not answer became a
// silently-accepted email chip (`docs/goal/ui/conversations.md` § Errors &
// edge cases → *The picker tells the truth*, 2026-08-29).
func (_self *ConversationsManager) AcceptCurrentRecipientChip() bool {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_conversations_fn_method_conversationsmanager_accept_current_recipient_chip(
			_pointer, _uniffiStatus)
	}))
}

func (_self *ConversationsManager) AcceptNewThreadChip(addr TypedAddress) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_accept_new_thread_chip(
			_pointer, FfiConverterTypedAddressINSTANCE.Lower(addr), _uniffiStatus)
		return false
	})
}

// `room-invitation-accept-button[i]` — accept the standing invitation
// `id` names: the account is seated on the room's floor, and the room
// opens as a thread and is selected. It reads nothing until a member with
// key authority keys it in, which happens on that member's device
// (`FaunaMlsBackend::tend_community_room`); the thread fills from then.
//
// Returns the room's thread, or `None` when the invitation is no longer
// standing or the accept was refused — the refusal on the page's
// `error-message`.
func (_self *ConversationsManager) AcceptRoomInvitation(id int64) *ThreadId {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	res, _ := uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_conversations_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) *ThreadId {
			return FfiConverterOptionalTypeThreadIdINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_accept_room_invitation(
			_pointer, FfiConverterInt64INSTANCE.Lower(id)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_rust_buffer(handle)
		},
	)

	return res
}

// Stage `bytes` as an attachment on `id`'s compose draft. Returns the
// `blob_hash` the bytes were cached under (the handle the client renders /
// the wire message references). Capability gating (`supports_attachments`)
// is the client's responsibility — a rail that drops attachments just won't
// inline them on `send`.
func (_self *ConversationsManager) AddAttachment(id ThreadId, filename string, mimeType string, bytes []byte) string {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_add_attachment(
				_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterStringINSTANCE.Lower(filename), FfiConverterStringINSTANCE.Lower(mimeType), FfiConverterBytesINSTANCE.Lower(bytes), _uniffiStatus),
		}
	}))
}

// Stage an attachment on the new-thread compose (`attachment-button` before
// a thread exists). Mirrors [`Self::add_attachment`] for the single-slot
// new-thread draft; `send_new_thread` carries the staged attachments onto
// the materialized thread's draft. `None` if no new-thread compose is open.
func (_self *ConversationsManager) AddNewThreadAttachment(filename string, mimeType string, bytes []byte) *string {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_add_new_thread_attachment(
				_pointer, FfiConverterStringINSTANCE.Lower(filename), FfiConverterStringINSTANCE.Lower(mimeType), FfiConverterBytesINSTANCE.Lower(bytes), _uniffiStatus),
		}
	}))
}

func (_self *ConversationsManager) AddObserver(obs SnapshotObserver) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_add_observer(
			_pointer, FfiConverterSnapshotObserverINSTANCE.Lower(obs), _uniffiStatus)
		return false
	})
}

// Add a participant to a thread.
//
// For `(FaunaMls, OneToOne)` this forks a new MLS group thread (the
// original 1:1 stays intact) — an MLS-protocol necessity: a pairwise
// MLS group can't gain a member, so the only path to a 3-person
// encrypted thread is a fresh group. Every other `(rail, flavor)` adds
// the participant in place: for SMTP this is just a wider CC, and for a
// bridge whose vector declares it, the far network's own add; there is
// no cryptographic fork. (A bridge declaring none has
// `supports_membership_change == false`, so this is unreachable for
// it.) Returns the (possibly new) thread id.
func (_self *ConversationsManager) AddParticipant(id ThreadId, addr TypedAddress) *ThreadId {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalTypeThreadIdINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_add_participant(
				_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterTypedAddressINSTANCE.Lower(addr), _uniffiStatus),
		}
	}))
}

func (_self *ConversationsManager) AddParticipantInner(id ThreadId, addr TypedAddress) *ThreadId {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalTypeThreadIdINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_add_participant_inner(
				_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterTypedAddressINSTANCE.Lower(addr), _uniffiStatus),
		}
	}))
}

// Add `addr` to the editable reply To line (`dm-reply-recipient-add`),
// de-duplicated by address identity. No-op if already present.
func (_self *ConversationsManager) AddReplyRecipient(id ThreadId, addr TypedAddress) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_add_reply_recipient(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterTypedAddressINSTANCE.Lower(addr), _uniffiStatus)
		return false
	})
}

// `room-settings-save-button` — commit every change the editor staged,
// each as its own policy commit, in [`RoomSettingsDraft::edits`]' order
// (the two rules, then the appointments and demotions, then the
// hand-over last). **Stops at the first refusal**, which the calls above
// have already painted on the page's `error-message`, and returns
// whether all of them landed — the editor closes only on `true`
// (`ui/conversations.md` § Element IDs, the `room_settings` sub-page).
//
// The whole loop lives here rather than in each app's Save handler so
// the seven agree on the order and on what "landed" means (priority #2);
// an app stages through [`RoomSettingsDraft`] and calls this once.
//
// [`RoomSettingsDraft`]: crate::room_settings::RoomSettingsDraft
// [`RoomSettingsDraft::edits`]: crate::room_settings::RoomSettingsDraft::edits
func (_self *ConversationsManager) ApplyRoomSettings(id ThreadId, edits []RoomSettingsEdit) bool {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	res, _ := uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) C.int8_t {
			res := C.ffi_fauna_conversations_rust_future_complete_i8(handle, status)
			return res
		},
		// liftFn
		func(ffi C.int8_t) bool {
			return FfiConverterBoolINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_apply_room_settings(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterSequenceRoomSettingsEditINSTANCE.Lower(edits)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_i8(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_i8(handle)
		},
	)

	return res
}

// Appoint a member of a governed room an admin (owner only,
// `conversation-rooms.md` § Roles and authorization).
func (_self *ConversationsManager) AppointAdmin(id ThreadId, addr TypedAddress) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_appoint_admin(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterTypedAddressINSTANCE.Lower(addr)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

// The shared attachment loader: the plaintext bytes cached under
// `blob_hash`, if any (populated by `add_attachment`, the send echo, and the
// inbound parse). The per-app render path resolves a
// `dm-attachment-image[i]` / `-file[i]` to real bytes through this. `None`
// for an unknown, not-yet-fetched, or **evicted** hash — the store is a
// bounded cache (`store::attachments`), and every app already paints the
// declared filename + size when the bytes are absent. A miss on a handle
// whose coordinates are remembered (a FaunaMls blob this device received or sent, a mail
// record, or either restored from a `history/<ch>` slice) marks it wanted
// and pokes the receive loop, whose next cycle fetches it again on that
// rail's sweep and notifies observers, so the same render asks again and
// hits.
func (_self *ConversationsManager) AttachmentBytes(blobHash string) *[]byte {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalBytesINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_attachment_bytes(
				_pointer, FfiConverterStringINSTANCE.Lower(blobHash), _uniffiStatus),
		}
	}))
}

// Whether `blob_hash`'s bytes are resident in this device's attachment store
// right now — a read-only peek that neither copies the bytes nor marks a miss
// wanted. An app that reuses a bubble across renders keys its rebuild on this
// as well as on the message: evicting bytes or fetching them again changes no
// message, so a bubble keyed on the message alone keeps painting what it
// painted before (`conversations.md` § Attachments → *Retention*).
func (_self *ConversationsManager) AttachmentResident(blobHash string) bool {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_conversations_fn_method_conversationsmanager_attachment_resident(
			_pointer, FfiConverterStringINSTANCE.Lower(blobHash), _uniffiStatus)
	}))
}

// Cache an inbound attachment's plaintext bytes under its `blob_hash`. The
// receive path ([`crate::backends::smtp::ingest_inbound_record`], and the
// wasm JS-driven poll's equivalent) calls this for each extracted attachment
// before ingesting the message, so the rendered bubble resolves the handle
// to real bytes via [`Self::attachment_bytes`].
//
// **The key is verified, not trusted.** `blob_hash` must be the lowercase-hex
// BLAKE3 of `bytes` — the invariant `conversations.md` § Attachments states —
// and a pair that does not satisfy it is **refused with a warn**, exactly as
// an undecryptable attachment is skipped. Silent rather than fallible on
// purpose: this method is `uniffi::export`ed, so a `Result` would change the
// FFI signature for all 7 apps to report a condition none of them can act on.
//
// Verifying here rather than at each caller is what makes the guarantee
// hold: this is the ONLY door into the store, it is a public FFI surface, and
// the store is a single map shared across every channel and room. Before
// this, `blob_hash` on the FaunaMls path was **sender-authored** (it rides
// inside the sealed `ChannelAttachment`) and unchecked, so any co-member who
// knew a hash could overwrite those bytes for every conversation at once —
// and, because `send` re-resolves bytes from this same map by hash
// (`resolve_attachments`), could swap a victim's *staged outgoing* file
// between staging and send, leaving them to seal and sign it under their own
// filename. Both arms close here: substituting bytes under a fixed key now
// requires a BLAKE3 preimage.
//
// Idempotent for a truthful pair — re-caching bytes under their own hash is
// a harmless overwrite (it is now genuinely the same value).
func (_self *ConversationsManager) CacheAttachmentBytes(blobHash string, bytes []byte) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_cache_attachment_bytes(
			_pointer, FfiConverterStringINSTANCE.Lower(blobHash), FfiConverterBytesINSTANCE.Lower(bytes), _uniffiStatus)
		return false
	})
}

// Close the add-participant overlay, discarding any in-progress input.
func (_self *ConversationsManager) CancelAddParticipant() {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_cancel_add_participant(
			_pointer, _uniffiStatus)
		return false
	})
}

func (_self *ConversationsManager) CancelNewConversation() {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_cancel_new_conversation(
			_pointer, _uniffiStatus)
		return false
	})
}

// The hex nest-channel id a FaunaMls thread is bound to (once its MLS group
// has bootstrapped / a Welcome materialized it), or `None` for an unbound
// thread. Surfaced into the e2e state protocol so a real-wire test can
// observe the channel an MLS conversation carries on the nest.
func (_self *ConversationsManager) ChannelHex(id ThreadId) *string {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_channel_hex(
				_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), _uniffiStatus),
		}
	}))
}

// Wipe every piece of identity-scoped state this manager holds, preserving
// registered backends and observers. Call it on the **production** account
// switch / sign-out path, before the incoming identity's session activates:
// the manager is an identity-scoped singleton that outlives the shell
// teardown, so a switch that skipped this would render the outgoing
// account's threads, drafts and selection to the incoming one.
//
// Deliberately NOT `_for_test`-gated. It is the production twin of the
// harness's [`Self::clear_for_test`] (which delegates here), and it exists
// because the two callers are genuinely different: the e2e harness wipes
// *between tests*, apple's `tearDownSessionForSwitch` and linux's
// identity-switch path wipe *between identities*. Apple reached the seam
// for the production job while every apple FFI recipe still shipped
// `test-helpers`, which `testing.md` § convention 15's recipe split removes
// — so the production path needs a production method, not a wider gate.
// Named to match the identity-change helpers linux already has
// (`critical_alerts::clear_for_identity_change`, `screen_lock::…`).
//
// It also advances [`Self::identity_epoch`], which retires every launch
// restore the outgoing account started ([`Self::restore_drafts_at`]).
func (_self *ConversationsManager) ClearForIdentityChange() {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_clear_for_identity_change(
			_pointer, _uniffiStatus)
		return false
	})
}

// Drop all registered observers. Each GTK client attaches a fresh observer
// per authenticated-window build (`views/conversations` → `observer::attach`),
// and nothing prunes the old ones — so across a sign-out→re-auth cycle they
// accumulate unboundedly, and (because the linux authenticated window's
// widget tree does not finalize on `destroy()`) the *stale* panes their
// receiver loops hold stay live in the a11y tree, re-presenting dialogs and
// fielding clicks against a now-shut-down runtime. Call this at sign-out
// (before the window is rebuilt): dropping the observers closes each stale
// loop's channel, so its `rx.recv()` returns `Err` and the loop breaks,
// releasing its panes. The next window build re-attaches a live observer.
func (_self *ConversationsManager) ClearObservers() {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_clear_observers(
			_pointer, _uniffiStatus)
		return false
	})
}

// Clear a previous gesture's error. Every producer calls this on entry: a
// new gesture supersedes the last one's outcome, so a success — or merely
// a retry in flight — never leaves the page accusing the user of a failure
// they have already moved past. A no-op (and **no** emit) when nothing is
// set, so the common path costs no observer tick.
func (_self *ConversationsManager) ClearPageError() {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_clear_page_error(
			_pointer, _uniffiStatus)
		return false
	})
}

func (_self *ConversationsManager) ClearSelection() {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_clear_selection(
			_pointer, _uniffiStatus)
		return false
	})
}

// Confirm the add-participant overlay (`thread-add-participant-button` →
// confirm). Snapshot: a `(FaunaMls, OneToOne)` add forks a fresh
// participant-keyed group thread (Signal semantics) and selects it; every
// other `(rail, flavor)` adds in place. Wire op: an in-place add on an
// already-bound FaunaMls group posts the MLS Commit + Welcome via the
// backend; a 1:1 fork bootstraps its group lazily on first `send`
// (Track B), and non-FaunaMls rails have no wire membership op. Returns
// the resulting thread id, or `None` if the overlay wasn't open / no
// address was picked.
func (_self *ConversationsManager) ConfirmAddParticipant() *ThreadId {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	res, _ := uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_conversations_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) *ThreadId {
			return FfiConverterOptionalTypeThreadIdINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_confirm_add_participant(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_rust_buffer(handle)
		},
	)

	return res
}

// The `data.conversation_sort` e2e state value, JSON-encoded — the list's
// active order in `setSort`'s serde spelling, from the same
// [`crate::state_json::conversation_sort_json`] tui and linux call in
// process, so no app spells the order names itself.
func (_self *ConversationsManager) ConversationSortJson() string {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_conversation_sort_json(
				_pointer, _uniffiStatus),
		}
	}))
}

// The `data.conversation_threads` e2e state rows, JSON-encoded — the
// manager-level twin of [`crate::session::ConversationsSession::conversation_threads_json`],
// for a caller that has a manager but no (or not yet activated) session:
// apple's e2e login path deliberately never activates a real
// `ConversationsSession` (`ConversationsVM.activate` — kept unset so the
// deterministic `test-helpers` mock backends stay in effect), so an app
// reading only off `session?.conversationThreadsJson()` sees an
// empty list forever regardless of what `inject_inbound_for_test` staged
// (a refactor regression — every apple conversations e2e test
// broke the same way, on both macOS and iOS, the moment it switched off
// the manager-backed row builder). Delegates to the same
// [`crate::state_json::conversation_threads_json`] free function, so the
// row shape (including `participant_actor_ids`) is identical either way.
func (_self *ConversationsManager) ConversationThreadsJson() string {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_conversation_threads_json(
				_pointer, _uniffiStatus),
		}
	}))
}

func (_self *ConversationsManager) DeactivateNewConversation() {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_deactivate_new_conversation(
			_pointer, _uniffiStatus)
		return false
	})
}

// `room-invitation-decline-button[i]` — the invitation stops standing for
// this account. The room is not told: a refusal the inviter could read
// would give declining an audience (`conversation-rooms.md` § Join rules
// and invites).
func (_self *ConversationsManager) DeclineRoomInvitation(id int64) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_decline_room_invitation(
			_pointer, FfiConverterInt64INSTANCE.Lower(id)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

// Delete `message` in `thread` (`dm-message-delete-button` —
// conversations.md § Reactions & message delete). FaunaMls-only, sender-
// only: no-op if the thread lacks `supports_message_delete`, or if the
// message is not owned by the local user. Optimistically tombstones the
// message in the manager-owned deleted set (re-emitting immediately), then
// fires the backend wire op best-effort — a wire failure is warned, not
// propagated; the optimistic state stands. The client renders the deleted
// placeholder off `MessageSnapshot.deleted`; body/attachments are left
// intact in the snapshot (the CLIENT decides the render, not the store).
func (_self *ConversationsManager) DeleteMessage(thread ThreadId, message MessageId) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_delete_message(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(thread), FfiConverterTypeMessageIdINSTANCE.Lower(message)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

// Demote an admin of a governed room to member (owner only).
func (_self *ConversationsManager) DemoteAdmin(id ThreadId, addr TypedAddress) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_demote_admin(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterTypedAddressINSTANCE.Lower(addr)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

// The whole conversations-rail draft set serialised to its canonical
// at-rest bytes. The client glue seals these under the owner's `BackupKey`
// and uploads them to the `__drafts` reserved folder after a compose
// change (`docs/goal/behavior/file-sync.md` § Drafts Sync). Byte-stable for
// equal logical state, so an unchanged draft set re-uploads identically.
func (_self *ConversationsManager) DraftsSnapshotBytes() []byte {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBytesINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_drafts_snapshot_bytes(
				_pointer, _uniffiStatus),
		}
	}))
}

// See [`Self::set_engine_served_elsewhere`]. Read by each app's
// `error-message` projection with top precedence — a standing condition
// that must never be masked by an unrelated gesture's `page_error`.
func (_self *ConversationsManager) EngineServedElsewhere() bool {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_conversations_fn_method_conversationsmanager_engine_served_elsewhere(
			_pointer, _uniffiStatus)
	}))
}

// Top up the local actor's one-time key-package pool on the nest to
// `target`. Thin FFI-facing wrapper over the FaunaMls rail backend's
// [`RailBackend::ensure_keypackages`](crate::backend::RailBackend::ensure_keypackages)
// (the backend owns the `MlsEngine` that mints packages — the manager stays
// free of MLS crypto per `conversations.md` § Architectural rules #2).
// Login-time replenish is session-owned
// ([`ConversationsSession::start_receive_loop`](crate::ConversationsSession::start_receive_loop)
// runs it **after** the MLS state-replica restore — a restore swaps the
// engine's provider storage, so a package minted before it would lose its
// private init key; `devices.md` § Cross-device MLS group-state sync);
// settings pages drive manual refresh through the same surface. Returns the
// number uploaded (`0` if at/above target, or if no FaunaMls backend is
// registered on this client). `target` of `0` is a count-only no-op.
//
// A mint writes fresh private init keys into the engine's provider storage
// only — this notifies the observers so the debounced replica autosave
// persists them (an unsaved mint leaves peers holding key packages whose
// init keys exist nowhere durable).
func (_self *ConversationsManager) EnsureKeypackages(target uint64) (uint64, error) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*BackendError](
		FfiConverterBackendErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
			res := C.ffi_fauna_conversations_rust_future_complete_u64(handle, status)
			return res
		},
		// liftFn
		func(ffi C.uint64_t) uint64 {
			return FfiConverterUint64INSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_ensure_keypackages(
			_pointer, FfiConverterUint64INSTANCE.Lower(target)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_u64(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_u64(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}

// Publish the actor's mandatory **last-resort** key package if absent. Thin
// FFI-facing wrapper over the FaunaMls rail backend's
// [`RailBackend::ensure_last_resort_keypackage`](crate::backend::RailBackend::ensure_last_resort_keypackage).
// Idempotent (the nest keeps a single last-resort row per actor), so every
// login calls it (session-owned, after the replica restore — see
// [`Self::ensure_keypackages`]) to stay `addressable` after the one-time
// pool drains (`docs/goal/architecture/federation.md` § Key packages).
// No-op if no FaunaMls backend is registered on this client. Notifies the
// observers on success — the backend mints a fresh package each call, and
// its private init key must reach the replica autosave like any other mint.
func (_self *ConversationsManager) EnsureLastResortKeypackage() error {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	_, err := uniffiRustCallAsync[*BackendError](
		FfiConverterBackendErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_ensure_last_resort_keypackage(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

	if err == nil {
		return nil
	}

	return err
}

// Remove `person` from **every** group of the owner's they are currently
// in — the *Remove* of the two review surfaces that render a person
// outside any one group (`identity-succession.md` § Propagation → *MLS
// groups*). See [`crate::eviction`] for why the operation lives here, and
// [`CrossGroupEviction::earned_verdict`] for what its outcome may and may
// not be recorded as.
//
// **Which groups: the ones the person is in *now*, re-derived — and
// re-derived from the authority, not from the chat snapshot.** The review
// item stores `(person, raising event, reason)` and deliberately not the
// groups, so this enumerates live membership. That is right for *removing*
// and would be wrong for *raising* — a raise is a fact about membership
// across the compromise window, and re-deriving it would flag people who
// joined afterwards (§ Propagation). Two operations, two different
// questions, one of which must never borrow the other's answer. *Which*
// live membership is the second half of the same rule, and it is the
// security-bearing half — see the contract note below.
//
// **A 1:1's honest peer is left alone**, and the gate is the thread
// *flavor* deliberately rather than the `supports_membership_change`
// capability the obvious draft reaches for. That capability cannot
// discriminate here: a `TypedAddress::Fauna` participant only ever appears
// on a FaunaMls thread, and **every** FaunaMls flavor is
// membership-capable, so the check is unreachable beside the actor-id
// match below — a guard that can never fire, and therefore one no test can
// ever pin (found by mutating it away and watching all three pins stay
// green). The reachable distinction is the one that matters anyway: a DM
// is not a group, and "removing" the only other person from a 1:1 would
// leave the owner alone in a thread the ordinary delete already handles —
// a second removal mechanism of exactly the kind § Propagation refuses to
// mint. What the flavor does **not** protect is a seat the 1:1 never
// promised — see rule (5) below.
// **⚠ Contract for every caller and every future edit: this driver's roster
// source must be the same source the flag it acts on was raised from.**
// That flag is `SweepReport::unattested_members`, read off the MLS **engine**
// (`MlsEngine::group_members`), so membership here is asked of the rail
// ([`crate::backend::RailBackend::authoritative_roster`]) and NOT of
// `ThreadDetail::participants`. The snapshot is a local view that no inbound
// Commit reconciles: where the two disagree — a foreign-authored add being
// the adversarial case — a snapshot-sourced enumeration `continue`s past the
// group, records no failure, and so earns `Removed` while the person is
// still seated, which is rule (3)'s harm reached through the enumeration
// door. A surface built on this must
// likewise never render *who remains* from the snapshot.
//
// **The span — § Propagation rule (5), ratified 2026-08-10.** The raise's
// span is every MLS group the engine holds; *Remove*'s reach is decided
// per channel class, and a raised seat it cannot clear rides
// [`CrossGroupEviction::unreachable`] as a typed fact — blocking the
// verdict, never escaping silently. Per class: a chat-**group** thread is
// evicted directly; a 1:1's one honest peer (the snapshot-listed
// participant) is never touched (rule (2)) but **any engine seat beyond
// the snapshot-listed peer is evicted like a group seat** — the chat poll
// applies membership Commits with no flavor gate, so a thief's Commit can
// seat a third identity in a DM channel; a chat channel with no bound
// thread here blocks as
// [`crate::eviction::UnreachableSeatClass::ChatGroupNoThreadHere`]; a
// folder channel blocks as
// [`crate::eviction::UnreachableSeatClass::FolderChannel`]
// (its removal is the folder plane's, and a left set converges the same
// way); a scheduling channel never blocks — no Commit is ever applied to
// one, so nothing can be planted there and nothing needs removing. The
// unbound enumeration follows the same source rule as the roster:
// [`crate::backend::RailBackend::unbound_seats_of`], off the engine.
func (_self *ConversationsManager) EvictPersonEverywhere(person ActorId) CrossGroupEviction {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	res, _ := uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_conversations_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) CrossGroupEviction {
			return FfiConverterCrossGroupEvictionINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_evict_person_everywhere(
			_pointer, FfiConverterTypeActorIdINSTANCE.Lower(person)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_rust_buffer(handle)
		},
	)

	return res
}

// Every Fauna identity on any of this member's thread rosters — each once,
// in thread-store order.
//
// The **rosters**, deliberately not [`Self::snapshot`]: that is the
// conversations page's view and applies the page's search filter, so a
// consumer walking it sees only the threads the search box currently
// shows. The peer-anchor harvest sweep is the caller this exists for —
// who it harvests must not depend on what the owner last typed into a
// filter, least of all since a succession statement can wait on that
// harvest (`identity-succession.md` § The succession statement → *the
// harvest wait*).
func (_self *ConversationsManager) FaunaRosterActors() []ActorId {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterSequenceTypeActorIdINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_fauna_roster_actors(
				_pointer, _uniffiStatus),
		}
	}))
}

// The handle `person` is currently seated under in the owner's own
// threads, for the two review surfaces that render a person **outside any
// one group** and therefore have no chip to read a name off.
//
// ⚠ **Display only, and never an identity claim.** The whole review exists
// because an identity a thief seated in a group may wear any handle it
// likes, so this is what to *show* the owner beside the row, never what to
// match on — every membership decision here keys on the actor id
// ([`crate::eviction::is_person`]).
//
// **`None` is an ordinary answer, not a failure**, and the permanent view
// is where it happens: that surface holds a backlog someone postponed, and
// a flagged person may have left every group since. A caller renders the
// row regardless — dropping it would hide an item nobody could then close,
// which is the failure this surface exists to prevent.
//
// Re-derived rather than stored, for [`Self::evict_person_everywhere`]'s
// reason: the review item deliberately keeps no group list, and a handle
// cached at raise time would go stale exactly when it matters (a rename
// between the sweep and the review).
// ⚠ **An empty display is `None`, not `Some("")`, and the skip happens
// DURING the search** (2026-08-10). A roster built from a Welcome carries
// no handles at all — `ingest_welcome` writes `handle: String::new()` for
// every member, because a leaf credential has none to give — so the
// seated-but-nameless row is the *ordinary* case for any group the owner
// joined rather than created, not a corner. Returning `Some("")` walked
// straight past every caller's no-name fallback (tui's
// `REVIEW_UNKNOWN_PERSON`) and rendered a review row with a blank where the
// person goes; the `None` arm was already written and already handled, and
// this is what makes it reachable. Filtering *inside* the scan rather than
// on its result is the load-bearing half: one person is commonly seated in
// several threads, and a nameless Welcome row would otherwise end the
// search and shadow a handle-bearing row one thread over.
//
// **Only a proven seat lends its handle** (`contacts.md` § The private
// overlay → *The paint gate*): a row is
// consulted only in a thread whose bound group holds `person` as a
// verified leaf ([`Self::proven_roster`]). A participant row's `(handle,
// actor id)` pair comes from a resolve answer the dial rule leaves unbound
// on the actor-id side, so a nest at the dialed domain that answers with
// this person's id would otherwise put ITS handle on the person's
// genuinely nameless seat one thread over — the same borrowed-identity
// harm as the nickname, reached through the backfill. A thread the rail
// keeps no roster for (not yet bootstrapped) lends nothing; once the group
// seats the person's own key, the handle the dialed domain routes to that
// key is theirs to lend.
func (_self *ConversationsManager) HandleForPerson(person ActorId) *string {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_handle_for_person(
				_pointer, FfiConverterTypeActorIdINSTANCE.Lower(person), _uniffiStatus),
		}
	}))
}

// The identity epoch: advanced by every [`Self::clear_for_identity_change`].
// A shell reads it **before** starting a detached read of account-scoped
// state (the launch `__drafts` fetch) and hands it back with the result
// ([`Self::restore_drafts_at`]), which refuses a result the outgoing
// account started. Opaque: only equality with a later read means anything.
func (_self *ConversationsManager) IdentityEpoch() uint64 {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterUint64INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_fauna_conversations_fn_method_conversationsmanager_identity_epoch(
			_pointer, _uniffiStatus)
	}))
}

// Ingest one decrypted inbound message: bucket it via the rail backend,
// resolve its thread (by participants / subject / reply-reference), and
// append it.
//
// **Message-ID dedup.** A message whose id is already held in the store is
// skipped (no second append). This is what keeps a server-side **Sent** copy
// from double-showing in-session: a first-party `fauna.email.send` is echoed
// locally on send (`Self::send`, keyed by the RFC `Message-ID` the client
// minted), and the SAME message then arrives back over `fauna.email.sent.fetch`
// as nest's durable Sent copy (`smtp-server.md` § Inbound client receive) — it
// carries that identical `Message-ID`, so the dedup suppresses the duplicate
// while leaving the message in the view. The caller's per-feed `seen` set
// (`poll_inbound_mail`) keys on the server *segment-record* id and so can't
// catch this (the local echo has no segment id); this id-level guard is the
// only thing that can. After a client restart the in-memory store is empty, so
// the Sent copy ingests normally and the sent message reloads — the durability
// the server-side copy exists for. Idempotent across overlapping re-polls too.
func (_self *ConversationsManager) IngestInbound(msg RailInboundMessage) error {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	_, _uniffiErr := rustCallWithError[*BackendError](FfiConverterBackendError{}, func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_ingest_inbound(
			_pointer, FfiConverterRailInboundMessageINSTANCE.Lower(msg), _uniffiStatus)
		return false
	})
	return _uniffiErr.AsError()
}

// **Leave the room `id` is** — the roles table's *leave (remove self)*
// row, and the gesture the departing-member report the nest has always
// admitted was missing a producer for (`conversation-rooms.md`
// § Roles and authorization → *Leaving — the mechanism*).
//
// **One verb, one door.** Every class leaves by the self-scoped
// `room.leave`, which stamps this account's floor row and no one else's.
// No app branches on class for this, and none should: the choice is the
// mechanism's, not the gesture's.
//
// **The owner is refused here, before the wire.** Both nest doors enforce
// "a room is never owner-less" on their own (one by rank, the other by
// refusing an owner-less roster for a floor that names an owner), so this
// check buys no safety — it buys the user a sentence that says what to do
// instead of a wire refusal that says a room was malformed. The paint is gated on
// `capabilities.can_leave_room` for the same reason, one layer up.
//
// **The thread stays.** This device keeps the generations and bubbles it
// already holds: deleting them would destroy the user's own copy of a
// conversation the user was legitimately part of, and would not un-read a
// byte.
//
// ⚠ **The leaver's own room verbs do NOT close yet** — declared, not
// overlooked (`conversation-rooms.md` § Implementation status today, the
// departed-render gap). `my_role` is read from the room's signed policy,
// which still names the leaver until a remaining owner or admin re-signs
// it, so this device keeps rendering the rank it held. Closing it wants a
// *departed* reading of the floor — this device has read the room's floor
// and it does not name this account — which also covers being removed by
// someone else, and is its own slice.
func (_self *ConversationsManager) LeaveRoom(id ThreadId) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_leave_room(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

// Read thread `id` without opening it. Opening one needs no call: the
// selection itself reads it ([`Self::notify`]), so an app that also calls
// this on select is redundant, never wrong.
func (_self *ConversationsManager) MarkRead(id ThreadId) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_mark_read(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), _uniffiStatus)
		return false
	})
}

// Number of registered snapshot observers. Diagnostic-only — surfaced through
// windows' e2e state protocol (`diagnostics.conversations_observer_count`) so a
// per-manager observer-accumulation regression (a client constructing a fresh
// observer on every re-navigation instead of reusing one for the manager's
// lifetime) fails as itself in a headless assertion instead of as a downstream
// dead-dispatch/retention symptom found later.
func (_self *ConversationsManager) ObserverCount() uint32 {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterUint32INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint32_t {
		return C.uniffi_fauna_conversations_fn_method_conversationsmanager_observer_count(
			_pointer, _uniffiStatus)
	}))
}

// Open the add-participant overlay for `id`. No-op if the thread
// doesn't exist. The picker starts empty (`Idle`).
//
// The thread lookup that answers "does it exist" also answers "does
// confirming reach the wire"
// ([`crate::capabilities::is_in_place_mls_group`]), so the overlay
// carries its own offline-gate discriminant from the moment it opens and
// no client has to re-derive it
// ([`AddParticipantState::in_place_mls_group`]).
func (_self *ConversationsManager) OpenAddParticipant(id ThreadId) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_open_add_participant(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), _uniffiStatus)
		return false
	})
}

// The page error the last membership/label gesture reported, as a plain
// `key: message` diagnostic — for a **test agent**, not for paint.
//
// [`Self::confirm_add_participant`], [`Self::remove_participant`] and
// [`Self::rename_thread`] are UI gestures: they report a failed wire op by
// stamping the page error and return `()` / `Option`, never a `Result`. An
// agent command that awaits one of them therefore has **no return value to
// check** — it must read this back, or it acks success for a wire op that
// failed, which is the convention-11 swallow (`e2e-conventions.md` §
// convention 11).
//
// `args["message"]` is what the *user* sees — the producers stamp it
// through `BackendError::user_detail`, so a product statement (a nest
// refusal, a version-mismatch sentence) reaches a test author verbatim,
// while a **diagnostic** reads as the generic sentence and its detail is in
// the log line beside the stamp. That is the send-slot taxonomy applying to
// this element's other producer, not a gap: a diagnostic is by definition
// not renderable, so a reader that needs the raw text needs the log.
func (_self *ConversationsManager) PageErrorDiagnostic() *string {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_page_error_diagnostic(
				_pointer, _uniffiStatus),
		}
	}))
}

// Whether the receive loop currently serving this manager **died by
// panic** — a standing condition each app's `error-message` projection
// shows directly under [`Self::engine_served_elsewhere`], telling the user
// to restart the app (`ui/conversations.md` § Errors & edge cases). Why a
// restart and not a re-armed loop: `ReceiveLoopExit::Panicked`.
//
// Only the *current* loop counts: a newer loop started over this manager
// clears it, and a superseded loop dying late never sets it. A designed
// exit (the session dropped, the engine handed over) never sets it at all.
// Lock-free, so a page render can never panic on a lock the dead pass
// poisoned.
func (_self *ConversationsManager) ReceiveStopped() bool {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_conversations_fn_method_conversationsmanager_receive_stopped(
			_pointer, _uniffiStatus)
	}))
}

// Remove the staged attachment at `index` from `id`'s compose draft (the
// `dm-attachment-*` remove affordance on the compose bar). The cached bytes
// are left in the store — harmless (in-memory, cleared on next login) and
// keeps the remove cheap; a shared blob could still be referenced elsewhere.
func (_self *ConversationsManager) RemoveAttachment(id ThreadId, index uint32) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_remove_attachment(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterUint32INSTANCE.Lower(index), _uniffiStatus)
		return false
	})
}

// Remove the staged attachment at `index` from the new-thread compose.
func (_self *ConversationsManager) RemoveNewThreadAttachment(index uint32) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_remove_new_thread_attachment(
			_pointer, FfiConverterUint32INSTANCE.Lower(index), _uniffiStatus)
		return false
	})
}

// Remove a participant (`thread-member-chip[i]` →
// `manager.remove_participant`). Snapshot: drop the participant. Wire op:
// a bound FaunaMls group posts an MLS Commit that re-keys the group so the
// removed member can't follow forward (no Welcome). Non-FaunaMls rails are
// snapshot-only.
func (_self *ConversationsManager) RemoveParticipant(id ThreadId, addr TypedAddress) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_remove_participant(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterTypedAddressINSTANCE.Lower(addr)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

// Remove `addr` from the editable reply To line
// (`dm-reply-recipient-remove`). Drops the recipient from *this reply
// only* — thread participants and history are untouched.
func (_self *ConversationsManager) RemoveReplyRecipient(id ThreadId, addr TypedAddress) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_remove_reply_recipient(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterTypedAddressINSTANCE.Lower(addr), _uniffiStatus)
		return false
	})
}

// Rename a thread (`thread-rename-button` → `manager.rename_thread`).
// Snapshot: relabel immediately. Wire op: a bound FaunaMls group posts an
// encrypted `GroupMeta::NameChanged` application message so peers apply the
// rename via `poll_inbound_conv` → [`Self::apply_inbound_rename`].
func (_self *ConversationsManager) RenameThread(id ThreadId, newLabel string) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_rename_thread(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterStringINSTANCE.Lower(newLabel)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

// What thread `id`'s compose bar previews for the reply in progress
// (`dm-reply-preview`): the answered message's sender and a plain-text
// excerpt, resolved against the thread's fetched messages. `None` when no
// reply is armed — and when the answered message is not in the fetched
// window, since an empty preview beats a stale or wrong one. One
// derivation for every app: until 2026-09-21 tui and apple painted the
// body, windows the sender's name, and web, android and linux the bare
// message id.
func (_self *ConversationsManager) ReplyPreview(id ThreadId) *ReplyPreview {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalReplyPreviewINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_reply_preview(
				_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), _uniffiStatus),
		}
	}))
}

// Resolve link-preview metadata (render-model.md § D4) for the bare `url` carried by a
// `RenderBlock::LinkPreview { Resolving }` block in a loaded bubble — the conversations
// twin of `FeedManager::resolve_link_preview`. Calls `fauna.linkpreview.resolve` once per
// URL (the result is cached in [`resolved_previews`](Self::resolved_previews) keyed by
// URL), maps the reply onto `PreviewState`, and notifies so the next
// [`thread_detail`](Self::thread_detail) folds the matching block `Resolved`/`Failed`. A
// transport error **or** an explicit `Failed` both collapse to the render model's terminal
// `Failed` (the bubble falls back to the plain inline link, § D4). Idempotent: a repeat
// call for an already-resolved URL is a no-op with NO further notify — render-loop-safe, so
// a non-fire-once client observer re-calling from a notify-driven re-render gets a no-op. A
// no-op too when no [`LinkPreviewRpc`] is wired (receive-only / SMTP-only).
func (_self *ConversationsManager) ResolveLinkPreview(url string) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_resolve_link_preview(
			_pointer, FfiConverterStringINSTANCE.Lower(url)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

// Resolve the active recipient picker's current raw input across the
// registered rail backends, then move its `resolve_state` Resolving →
// Resolved / NotFound / Error and stash the resolved [`TypedAddress`] in
// `RecipientPickerState::resolved`. A following
// [`Self::accept_current_recipient_chip`] commits that *resolved* address
// (e.g. a 64-hex actor id promoted to `Fauna` by the FaunaMls key-package
// probe), not the format-only parse. The add-participant overlay's picker
// takes priority over the new-thread picker, matching
// `accept_current_recipient_chip`. No-op if neither picker is open or the
// input is empty/whitespace.
func (_self *ConversationsManager) ResolveRecipient() {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_resolve_recipient(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

// Restore the draft set from bytes the client glue fetched from `__drafts`
// and unsealed with the owner's `BackupKey` — the load-on-launch /
// cross-device catch-up path. Refreshes observers so the composer reflects
// the restored drafts *before* the probe below runs. A corrupt or
// unreadable blob is logged and ignored (start from empty drafts) rather
// than failing the surface.
//
// The restore **fills**: it never clears a slot and never overwrites a
// draft the user is composing into, however late the fetch lands
// ([`DraftStore::restore_from_bytes`] owns the rule and the reasons).
//
// **Why this is `async` — a restore owes the recipient picker a probe.**
// `conversations.md` § Errors & edge cases → *The picker tells the truth*
// rule 1 ratifies that a non-empty recipient input is never `Idle`: typing
// stamps `Resolving` and only [`Self::resolve_recipient`] moves it on. But
// what *rests* is `raw_input` without the probe output — a running probe
// cannot survive a relaunch, and resting `Resolving` would paint a spinner
// nothing ever resolves (`store::drafts::persistable_picker` owns that
// reasoning). So a restored picker arrives holding an address at `Idle`,
// which was a **dead state**: nothing re-probes it, no chip can be
// committed from it (`federation.md` § Peer-auth model → *Discovery-failure
// semantics* — a chip is only ever a probed address), and the user cannot
// even re-arm it by re-typing the same address, because every app shell
// suppresses the echo of an unchanged field. Restoring the invariant in
// each of the seven shells would be seven copies of the rule (priorities
// #1/#2), so the restore issues the probe itself and every app gets it by
// calling the one method it already calls.
//
// This is the restore at the **current** [`Self::identity_epoch`]. A shell
// whose fetch can straddle an identity change on a manager it keeps across
// the switch calls [`Self::restore_drafts_at`] with the epoch it captured
// before the fetch instead.
func (_self *ConversationsManager) RestoreDrafts(bytes []byte) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_restore_drafts(
			_pointer, FfiConverterBytesINSTANCE.Lower(bytes)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

// [`Self::restore_drafts`] for a fetch the shell started at `epoch` (read
// from [`Self::identity_epoch`] **before** the fetch). If
// [`Self::clear_for_identity_change`] has run since, the bytes belong to
// the outgoing account and the call does nothing: no fill, no notify, and
// no recipient probe, so the incoming account's drafts are not shadowed,
// its next autosave does not re-seal the outgoing account's text into its
// own plane, and its rails are never asked to resolve the outgoing
// account's recipient (`account-scoping.md` § The scoping taxonomy — the
// writers of account-scoped state are retired by the same drop). The
// refusal comes before the fill because the fill is what arms the probe.
func (_self *ConversationsManager) RestoreDraftsAt(epoch uint64, bytes []byte) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_restore_drafts_at(
			_pointer, FfiConverterUint64INSTANCE.Lower(epoch), FfiConverterBytesINSTANCE.Lower(bytes)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

// Retire the currently-registered conversations-engine rail
// ([`Rail::FaunaMls`]) so a successor can take its place: drop the
// registration and call [`RailBackend::retire`] on the way out, which
// releases the MLS engine's conversations-engine role lock over
// `mls_state.db`.
//
// **Why this is a separate method and not part of
// [`Self::clear_for_identity_change`].** That one deliberately *preserves*
// registered backends — its doc says so, and it is right to: it wipes the
// identity-scoped *content* a switch must not carry over, while the rails
// themselves are re-registered a moment later by the incoming session. This
// method does the opposite and rarer thing, and the two run at different
// moments: the wipe happens at the identity change, the retire happens
// immediately before the successor engine is constructed. Folding them
// together would leave a manager with no rails through every switch, and
// would still miss the case this exists for — a **same-identity** re-login,
// where nothing is identity-changing but a second engine over one
// `mls_state.db` is refused all the same.
//
// Called by the shared native session factory
// (`fauna_ffi::FfiNestClient::conversations_session*`) before
// `MlsEngine::new`, so every UniFFI app — macOS, iOS, windows, android —
// inherits the hand-over with no glue of its own. Idempotent, and a no-op
// when no MLS rail is registered (a bare or mock-backed manager).
//
// The window between this call and the successor's `register_backend` is
// deliberate and bounded: it is one engine construction wide, and during it
// a send on the MLS rail resolves no backend and fails honestly, which is
// the correct answer while the account's engine is mid-hand-over.
//
// **Also exported over UniFFI, for the shell that DROPS its manager**
// (2026-09-02). "Every UniFFI app inherits the hand-over with no glue of
// its own" was true only of shells that keep one manager across the
// hand-over, because the factory can only retire the manager it is *handed*.
// Windows replaces its process-wide manager at an actor change
// (`ConversationsManagerHost.ResetForActorChange` — its sanctioned
// exception to the no-swap rule, since the outgoing identity's rails,
// observers and threads must not survive a switch), and it does so
// **before** the successor build — so the factory's retire ran against a
// brand-new manager with no MLS rail, took the documented no-op arm, and
// the predecessor engine was left to a reference drop. That is the
// mechanism this ruling refuses. A shell that drops its manager therefore
// calls this on the OUTGOING one first; the call is the same explicit
// ordered hand-over, moved to the one seam the factory cannot see.
func (_self *ConversationsManager) RetireConversationsEngine() {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_retire_conversations_engine(
			_pointer, _uniffiStatus)
		return false
	})
}

// Opt this message into loading its remote images. Adds it to the in-memory
// reveal set and re-emits, so the next [`thread_detail`](Self::thread_detail)
// projects `RemoteImage.revealed: true` for it. **In-memory only** — the
// no-persistence posture (html-mail.md § Rendering) is unchanged; a reveal
// does not survive a restart. Idempotent: a repeat tap is a no-op insert
// plus a harmless re-emit.
func (_self *ConversationsManager) RevealRemoteImages(messageId MessageId) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_reveal_remote_images(
			_pointer, FfiConverterTypeMessageIdINSTANCE.Lower(messageId), _uniffiStatus)
		return false
	})
}

// The address to seat for a roster actor id — the **one** resolution path
// both roster seating sites use (`conversation-rooms.md` § Implementation
// status today: "resolving a seated actor id to a handle — one follow-on
// serving both this arm and the Welcome's").
//
// `backends::fauna_mls::ingest_welcome` (the members a Welcome brings) and
// [`Self::apply_inbound_roster`] (a member another device added) both learn
// their members from the MLS engine roster, which carries actor ids and
// nothing else. Each used to seat `handle: String::new()` inline — so a
// member arrived nameless even when this very device was already rendering
// that same person, by name, one thread over.
//
// This resolves against that device-local knowledge
// ([`Self::handle_for_person`]) and seats the handle when it finds one.
// Three properties it is chosen for:
//
// - **Non-blocking, no I/O.** A pure in-memory scan of threads this device
// already holds, so it is safe on [`Self::apply_inbound_roster`]'s path
// — which runs inside the inbound poll's channel lock — and safe under
// the e2e state provider's no-blocking-I/O rule (e2e convention 11's
// corollary).
// - **Honest when it finds nothing.** The seat keeps its empty handle and
// [`TypedAddress::display`] renders the actor's short id, so an
// unresolved member is a member with an elided name rather than a blank
// row.
// - **Never an identity claim.** A handle read off another thread is what
// to *show*; every membership decision still keys on the actor id
// ([`TypedAddress::same_participant`]) — [`Self::handle_for_person`]
// carries the full argument.
//
// An actor this device has never met resolves to nothing today. The
// remaining leg is a *network* id-keyed handle read: neither the floor
// roster's member records
// (`fauna_protocol::conversations::RoomRosterMemberWire`) nor
// `fauna.profile.get` carries a handle to serve it yet.
func (_self *ConversationsManager) SeatAddressFor(actor ActorId) TypedAddress {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterTypedAddressINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_seat_address_for(
				_pointer, FfiConverterTypeActorIdINSTANCE.Lower(actor), _uniffiStatus),
		}
	}))
}

// The Status page's `status-mls-channels` count —
// [`crate::snapshot::secure_channel_count`] over the live thread store,
// under the same lock discipline as [`Self::unread_total`] (a read, never
// inline on a mutator's thread). The shared snapshot's MLS leg
// (`ui/status.md` § State & data shape) takes this number as its input.
func (_self *ConversationsManager) SecureChannelCount() uint64 {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterUint64INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_fauna_conversations_fn_method_conversationsmanager_secure_channel_count(
			_pointer, _uniffiStatus)
	}))
}

func (_self *ConversationsManager) SelectThread(id ThreadId) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_select_thread(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), _uniffiStatus)
		return false
	})
}

// Select a thread **and** one message inside it — the whole of
// `SearchNav::Mail`'s contract (`docs/goal/ui/search.md` § State & data
// shape), and the only producer of a message selection.
//
// One write pair + one [`Self::notify`], so observers never see the
// intermediate state where the thread has flipped but the message has not:
// on an app that scrolls to the selection, that intermediate frame is a
// visible jump to the wrong place.
//
// The `message_id` is **not** validated here. The thread's messages arrive
// asynchronously, so a hit on a not-yet-fetched message would fail a
// constructor-time check and lose a selection that becomes valid moments
// later; [`Self::thread_detail`] resolves it against the live window on
// every emit instead, which also makes a late arrival light up by itself.
func (_self *ConversationsManager) SelectThreadAndMessage(threadId ThreadId, messageId MessageId) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_select_thread_and_message(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(threadId), FfiConverterTypeMessageIdINSTANCE.Lower(messageId), _uniffiStatus)
		return false
	})
}

// Send the per-thread compose draft for an existing thread, routing
// through the thread's rail backend
// (`docs/goal/ui/conversations.md` § User actions:
// `dm-send-button → manager.send(thread_id)`). On success appends the
// sent message and clears the draft; on failure stamps
// `ComposeState.send_state = Failed { reason }` and returns the error.
func (_self *ConversationsManager) Send(id ThreadId) error {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	_, err := uniffiRustCallAsync[*BackendError](
		FfiConverterBackendErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_send(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

	if err == nil {
		return nil
	}

	return err
}

// Materialize the new-thread compose into a real thread, then send it
// (`dm-send-button → manager.send_new_thread()` for new-thread compose).
// Returns the new thread id, or `None` when there is no active
// new-thread compose / no committed recipient chip. On send failure the
// thread is left materialized with a `Failed` draft (the user can retry).
func (_self *ConversationsManager) SendNewThread() (*ThreadId, error) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*BackendError](
		FfiConverterBackendErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_conversations_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) *ThreadId {
			return FfiConverterOptionalTypeThreadIdINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_send_new_thread(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_rust_buffer(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}

// Update the add-participant picker's raw input. No-op if the overlay
// isn't open. Same *typing owes a probe* rule as
// [`Self::set_new_thread_recipient_input`].
func (_self *ConversationsManager) SetAddParticipantRecipientInput(text string) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_add_participant_recipient_input(
			_pointer, FfiConverterStringINSTANCE.Lower(text), _uniffiStatus)
		return false
	})
}

func (_self *ConversationsManager) SetComposeBody(id ThreadId, body string) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_compose_body(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterStringINSTANCE.Lower(body), _uniffiStatus)
		return false
	})
}

func (_self *ConversationsManager) SetComposeSubject(id ThreadId, subject string) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_compose_subject(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterStringINSTANCE.Lower(subject), _uniffiStatus)
		return false
	})
}

// Record whether this process is a non-holder of the conversations-engine
// role (`MlsError::ServedElsewhere` at engine construction). Called by the
// app's engine-construction site on every attempt — success clears it,
// `ServedElsewhere` sets it, any other engine-init failure leaves it
// cleared (a different, unrelated failure). A no-op (no `notify`) when
// the value is unchanged, mirroring [`Self::clear_page_error`]'s
// no-op-when-unset optimization.
func (_self *ConversationsManager) SetEngineServedElsewhere(served bool) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_engine_served_elsewhere(
			_pointer, FfiConverterBoolINSTANCE.Lower(served), _uniffiStatus)
		return false
	})
}

func (_self *ConversationsManager) SetNewThreadBody(body string) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_new_thread_body(
			_pointer, FfiConverterStringINSTANCE.Lower(body), _uniffiStatus)
		return false
	})
}

// `recipient-picker-home-nest-toggle` — whether the room about to be
// created seats the user's home nest, which makes it a **community** room
// the first send founds (`conversation-rooms.md` § The three classes).
// The picker's class statement follows it
// ([`crate::room::prospective_room_class`]). A no-op with no new-thread
// compose open.
func (_self *ConversationsManager) SetNewThreadHomeNest(include bool) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_new_thread_home_nest(
			_pointer, FfiConverterBoolINSTANCE.Lower(include), _uniffiStatus)
		return false
	})
}

// Update the new-thread picker's raw input. **Typing owes a probe**: any
// non-empty input parks the picker on `Resolving` until the async
// [`Self::resolve_recipient`] reports; empty input is `Idle`. The state is
// never derived from the text's *shape* — a shape-derived `Resolved` said
// "Resolved" for a Fauna peer whose lookup had not started, and let Enter
// commit an email chip for it (`docs/goal/ui/conversations.md` § Errors &
// edge cases → *The picker tells the truth*, 2026-08-29).
func (_self *ConversationsManager) SetNewThreadRecipientInput(text string) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_new_thread_recipient_input(
			_pointer, FfiConverterStringINSTANCE.Lower(text), _uniffiStatus)
		return false
	})
}

func (_self *ConversationsManager) SetNewThreadSubject(subject *string) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_new_thread_subject(
			_pointer, FfiConverterOptionalStringINSTANCE.Lower(subject), _uniffiStatus)
		return false
	})
}

// Stamp [`ConversationsSnapshot::error`] and emit, so the failure reaches
// `error-message` on the very tick that produced it. Emitting here — at the
// *event* — rather than leaving it for a caller's later `notify` is what
// makes the surface hold for `remove_participant`/`rename_thread`, whose
// own `notify` fires **before** their wire op runs.
func (_self *ConversationsManager) SetPageError(error fauna_core.LocalizedText) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_page_error(
			_pointer,
			CFromRustBuffer(fauna_core.FfiConverterLocalizedTextINSTANCE.LowerExternal(error)), _uniffiStatus)
		return false
	})
}

func (_self *ConversationsManager) SetReplyTo(id ThreadId, msg *MessageId) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_reply_to(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterOptionalTypeMessageIdINSTANCE.Lower(msg), _uniffiStatus)
		return false
	})
}

// Change a governed room's history policy (`conversation-rooms.md`
// § History for joiners) — the policy editor's second field. Owner or
// admin.
func (_self *ConversationsManager) SetRoomHistoryPolicy(id ThreadId, policy HistoryPolicy) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_room_history_policy(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterHistoryPolicyINSTANCE.Lower(policy)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

// Change a governed room's join rule (`conversation-rooms.md` § Join
// rules and invites) — the policy editor's first field. Owner or admin;
// a refusal surfaces on the page's `error-message` like every other
// membership gesture.
func (_self *ConversationsManager) SetRoomJoinRule(id ThreadId, rule JoinRule) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_room_join_rule(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterJoinRuleINSTANCE.Lower(rule)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

// Replace the transparent labelers a community room's home nest applies
// to its messages — `labelers` are published labeler ids, lowercase hex
// (`conversation-rooms.md` § The three classes → *What the home nest does
// with its read*, purpose 2). Owner or admin; a refusal surfaces on the
// page's `error-message` like every other policy gesture.
func (_self *ConversationsManager) SetRoomLabelers(id ThreadId, labelers []string) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_room_labelers(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterSequenceStringINSTANCE.Lower(labelers)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

// Grant or withdraw the home nest's read of the community room `id` is —
// the editor's `room-nest-read-toggle`, committed on Save through
// [`Self::apply_room_settings`]. A key rotation, and so the owner's or an
// admin's act; a refusal lands on the page's `error-message` like any
// other room-settings failure.
func (_self *ConversationsManager) SetRoomNestRead(id ThreadId, reads bool) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_room_nest_read(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterBoolINSTANCE.Lower(reads)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

func (_self *ConversationsManager) SetSearchQuery(query *string) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_search_query(
			_pointer, FfiConverterOptionalStringINSTANCE.Lower(query), _uniffiStatus)
		return false
	})
}

func (_self *ConversationsManager) SetSort(order SortOrder) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_set_sort(
			_pointer, FfiConverterSortOrderINSTANCE.Lower(order), _uniffiStatus)
		return false
	})
}

func (_self *ConversationsManager) Snapshot() ConversationsSnapshot {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterConversationsSnapshotINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_snapshot(
				_pointer, _uniffiStatus),
		}
	}))
}

func (_self *ConversationsManager) StartNewConversation() {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_start_new_conversation(
			_pointer, _uniffiStatus)
		return false
	})
}

// Seed a reply draft on `id` to `msg_id` (`dm-reply-button` /
// `dm-reply-all-button`). Always sets `compose.reply_to`. On rails with
// `supports_recipient_selection` (mail) it also seeds the editable To line
// (`compose.reply_recipients`): `reply_all == false` → the replied
// message's sender only; `reply_all == true` → every thread participant
// except the local user (`backend.self_address()`). On other rails the To
// line is hidden, so `reply_recipients` stays empty (recipients ARE the
// thread membership). Either seed is then editable via
// [`Self::add_reply_recipient`] / [`Self::remove_reply_recipient`]
// (`conversations.md` § Participants vs reply recipients).
func (_self *ConversationsManager) StartReply(id ThreadId, msgId MessageId, replyAll bool) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_start_reply(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterTypeMessageIdINSTANCE.Lower(msgId), FfiConverterBoolINSTANCE.Lower(replyAll), _uniffiStatus)
		return false
	})
}

func (_self *ConversationsManager) ThreadDetail(id ThreadId) *ThreadDetail {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalThreadDetailINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationsmanager_thread_detail(
				_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), _uniffiStatus),
		}
	}))
}

// Toggle a reaction on `message` in `thread` (`dm-reaction-*` /
// `dm-reaction-add` — conversations.md § Reactions & message delete).
// FaunaMls-only: no-op if the thread has no `supports_reactions` capability
// or the local actor is unknown. Optimistically pushes an Add or Remove
// event onto the manager-owned reaction log (re-emitting so clients see the
// update immediately), then fires the backend wire op best-effort — a wire
// failure is warned, not propagated; the optimistic state stands.
func (_self *ConversationsManager) ToggleReaction(thread ThreadId, message MessageId, emoji string) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_toggle_reaction(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(thread), FfiConverterTypeMessageIdINSTANCE.Lower(message), FfiConverterStringINSTANCE.Lower(emoji)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

func (_self *ConversationsManager) ToggleTopic(id ThreadId) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_toggle_topic(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), _uniffiStatus)
		return false
	})
}

// Hand a governed room to another member (owner only,
// `conversation-rooms.md` § Roles and authorization → *Ownership
// transfer*). The owner's act posts the countersigned offer; the roles
// flip on every seat once the new owner's device has committed it, so
// the projection follows the agreed group context — not this call.
func (_self *ConversationsManager) TransferRoomOwnership(id ThreadId, addr TypedAddress) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_transfer_room_ownership(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterTypedAddressINSTANCE.Lower(addr)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}

// How many received mail records this process skipped because they would
// not open under the account's complete standing key set
// ([`Self::note_unopenable_mail`]) — a standing truth each app's
// `error-message` projection shows below every other page error
// (`ui/conversations.md` § Errors & edge cases): the mailbox keeps
// receiving past such a record, and the user is told that some mail did
// not open on this device. `0` clears it. Not cleared by any gesture; a
// record opening later (a re-drain after the account's keys changed)
// retires its entry.
func (_self *ConversationsManager) UnopenableMailCount() uint32 {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterUint32INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint32_t {
		return C.uniffi_fauna_conversations_fn_method_conversationsmanager_unopenable_mail_count(
			_pointer, _uniffiStatus)
	}))
}

// The home-screen widget's number (`apps/common.md` § Home-screen
// widget): [`crate::snapshot::sum_unread`] over **every** thread of the
// account — the unfiltered store, not [`Self::snapshot`]'s list, which a
// typed search narrows. A widget outside the app reports the account's
// unread, and a transient filter inside the app must not move it. One
// getter for every app's outside-the-app surface, so none keeps a tally of
// its own or runs a second count query. Reads the thread store under its
// own lock, so — like every snapshot read — an observer calls it after
// the notifying mutation unwinds, never inline on the mutator's thread.
func (_self *ConversationsManager) UnreadTotal() uint32 {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterUint32INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint32_t {
		return C.uniffi_fauna_conversations_fn_method_conversationsmanager_unread_total(
			_pointer, _uniffiStatus)
	}))
}

// Withdraw an invitation pending on the room `id` is — the gesture every
// row of [`crate::room::RoomSnapshot::pending_invites`] carries
// (`conversation-rooms.md` § Join rules and invites → *Pending invitations
// are visible to whoever may withdraw them*). `invitee_actor_hex` is the
// row's own `RoomPendingInviteSnapshot::invitee_actor_hex`.
//
// Acts at once, never staged through the editor's Save: a withdrawal is
// not a policy edit. No app gates it — the home nest served the row to
// this viewer *because* this viewer may withdraw it, and judges the act
// again at its own door. The invitee is told nothing. On success the list
// has been read again and the row is gone; a refusal lands on the page's
// `error-message`.
func (_self *ConversationsManager) WithdrawRoomInvite(id ThreadId, inviteeActorHex string) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsManager")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationsmanager_withdraw_room_invite(
			_pointer, FfiConverterTypeThreadIdINSTANCE.Lower(id), FfiConverterStringINSTANCE.Lower(inviteeActorHex)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}
func (object *ConversationsManager) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterConversationsManager struct{}

var FfiConverterConversationsManagerINSTANCE = FfiConverterConversationsManager{}

func (c FfiConverterConversationsManager) Lift(handle C.uint64_t) *ConversationsManager {
	result := &ConversationsManager{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_fauna_conversations_fn_clone_conversationsmanager(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_fauna_conversations_fn_free_conversationsmanager(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*ConversationsManager).Destroy)
	return result
}

func (c FfiConverterConversationsManager) Read(reader io.Reader) *ConversationsManager {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterConversationsManager) Lower(value *ConversationsManager) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*ConversationsManager")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterConversationsManager) Write(writer io.Writer, value *ConversationsManager) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalConversationsManager(handle uint64) *ConversationsManager {
	return FfiConverterConversationsManagerINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalConversationsManager(value *ConversationsManager) uint64 {
	return uint64(FfiConverterConversationsManagerINSTANCE.Lower(value))
}

type FfiDestroyerConversationsManager struct{}

func (_ FfiDestroyerConversationsManager) Destroy(value *ConversationsManager) {
	value.Destroy()
}

// A FaunaMls-wired conversations session for the native UniFFI clients.
//
// Constructed via [`Self::from_parts`] (plain Rust — takes a `dyn
// ConversationsRpc` for dependency injection, so tests pass a mock nest; the FFI
// factory in `fauna-ffi` passes the real `NestConversationsRpc`). The session
// owns the wired [`ConversationsManager`] (the single observable surface the
// client drives for snapshot/send/rename/…) and the concrete
// [`FaunaMlsBackend`] the receive free functions need by `&` reference.
type ConversationsSessionInterface interface {
	// This session's receive-cycle counters, JSON-encoded (`{"started": N,
	// "completed": M, "exit": null | "closed" | "retired" | "panicked"}`) —
	// the UniFFI twin of
	// [`crate::state_json::conv_receive_cycles_json`] for apps that cross the
	// FFI boundary (that function's `Option<&Arc<Self>>` signature isn't
	// callable from a `&self` method; this computes the identical shape from
	// the same [`ReceiveCycles`] getters — [`Self::receive_cycles`] owns the
	// contract, this is not a second count). android's TestAgent re-parses the
	// string into its own state object (the established `machine_method_result`
	// passthrough shape, `TestAgent.kt::serializeState`) rather than
	// re-deriving the counts in Kotlin; tui/linux/web call the free function
	// directly and never need this.
	ConvReceiveCyclesJson() string
	// UniFFI twin of [`Self::poke_receive_cycle`] — run one receive cycle now
	// (convention 14 mechanism 3, `e2e-conventions.md`). Native only, matching
	// [`Self::poke_receive_cycle`]'s own gate (wasm never spawns the detached
	// receive loop this pokes).
	ConvReceiveNow()
	// The `data.conversation_threads` e2e state rows, JSON-encoded — the
	// UniFFI twin of [`crate::state_json::conversation_threads_json`] for
	// apps that cross the FFI boundary (that function takes `&ConversationsManager`
	// directly, which a `&self` UniFFI method can supply from
	// [`Self::manager`] but a free function outside this crate cannot). Same
	// passthrough shape as [`Self::conv_receive_cycles_json`]: android's
	// TestAgent re-parses the string into its own state object rather than
	// re-deriving the row shape in Kotlin; tui/linux/web call the free
	// function (or, for web, the wasm twin) directly and never need this.
	ConversationThreadsJson() string
	// Process a same-nest MLS Welcome (`welcome_bytes` for `channel_id_hex`):
	// join + bind the group, materialize its thread. Idempotent — a re-delivered
	// Welcome for an already-bound channel returns the same thread id without a
	// second join. Returns `Some(thread_id)` (the materialized thread's id
	// string). Fed by the client's `fauna.conversations.welcome.received` push
	// handler. The native twin of the wasm wrapper's `ingestWelcome`; all MLS
	// crypto stays in [`FaunaMlsBackend`] (`docs/goal/ui/conversations.md` §
	// Architectural rules #2).
	IngestWelcome(channelIdHex string, welcomeBytes []byte) (*string, error)
	// The wired [`ConversationsManager`] — the single observable surface
	// (`docs/goal/ui/conversations.md` § State & data shape) the client drives
	// for snapshot / send / rename / membership / recipient resolution. The
	// FaunaMls backend is already registered on it.
	Manager() *ConversationsManager
	// This session's `mls_folded_commits` observable, JSON-encoded
	// (`{channel_hex: count}`) — the UniFFI twin of
	// [`crate::state_json::mls_folded_commits_json`], which this delegates to
	// directly ([`Self::backend`] already returns the `&FaunaMlsBackend` it
	// wants — no second count, unlike [`Self::conv_receive_cycles_json`]'s
	// `Arc<Self>` mismatch). android's TestAgent re-parses the string into its
	// own state object rather than re-deriving the counts in Kotlin.
	MlsFoldedCommitsJson() string
	// The session's retained post-decrypt **local detections**, newest-first — the
	// client half of the moderation queue (`docs/goal/behavior/moderation.md`
	// § Layout & flow). The queue VM unions these with the server
	// `fauna.moderation.actions` rows via `fauna_client_moderation::merge_queue`
	// (linux calls it directly; FFI/WASM clients over the shared façade). Empty
	// until the receive loop classifies an incoming spam message post-decrypt.
	ModerationLocalDetections() []fauna_client_moderation.LocalDetection
	// The decrypted plaintext body behind one **local-detection** queue row, by
	// its `content_id` (= the classified message's id) — the train-correction
	// text source for the client-side tier-1 spam-model write
	// (`MailSettingsMachine::train_spam_model_client`; the FFI twin of the
	// linux-native `manager().message_body(..)` read). `None` once the message
	// has aged out of the thread store — the correction then just clears the
	// flag, exactly as before the client write path existed.
	ModerationMessageBody(contentId string) *string
	// Drop the local detection for `content_id` after the user trains a correction
	// on that queue row (the train button → `fauna.moderation.train` also removes
	// the client-side row, since it is corrected). No-op if it is a server row (not
	// in this store) or already gone; returns `true` iff one was removed.
	ModerationRemoveLocalDetection(contentId string) bool
	// Poll every bound FaunaMls channel for new ciphertext (fetch → decode →
	// MLS-decrypt → ingest), advancing each channel's `seq` cursor. Returns the
	// total number of messages ingested this pass. The reconnect / missed-push
	// backstop the client's poll loop ticks — the native twin of the wasm
	// wrapper's `pollConversations`.
	//
	// A per-channel failure is non-fatal — the next tick retries the whole sweep
	// — so the loop keeps going and remembers the first error; it only returns
	// `Err` when nothing was ingested **and** something failed (otherwise the
	// partial success is the useful answer). The per-channel cursor is read +
	// copied before the `.await` and written back after, so the `conv_cursors`
	// lock is never held across the await.
	PollConversations() (uint32, error)
	// Poll both mail read-feeds (`INBOX` then `Sent`) for new sealed records
	// (fetch → decode → HPKE-decrypt → ingest), advancing each mailbox's
	// `after_uid` cursor. Returns the total ingested this pass. The reconnect /
	// missed-tick backstop a client may call directly — the mail twin of
	// [`Self::poll_conversations`]; [`Self::start_receive_loop`]'s ticker drives
	// it automatically. A no-op (`Ok(0)`) until [`Self::register_mail_receive`]
	// has wired the sources, or while mail stays unconfigured (the source returns
	// an empty page). Same partial-success contract as `poll_conversations`: only
	// `Err` when nothing was ingested **and** a mailbox failed. The cursor is
	// read + copied before the `.await` and written back after, so the
	// `mail_cursors` lock is never held across the await.
	PollMail() (uint32, error)
	// Update the logged-in account's canonical `<handle>@<domain>` — THE one
	// self-heal call (`docs/goal/ui/conversations.md` § State & data shape →
	// *Self-address: live, never baked*). Both rails read the live cell at use
	// time, so the next SMTP send carries this `From:` (a session built before
	// identity resolution stops refusing `no_handle`), the FaunaMls data plane
	// routes same-nest peers against this domain, and the reply-all self-drop
	// compares against this address. Call it from wherever identity state
	// lands: login-time resolution, the background identity refresh, a
	// server-side handle rename. Idempotent; nothing is rebuilt.
	SetSelfAddress(selfAddress string)
	// Start the detached push-driven receive loop and return immediately. The
	// native twin of the linux `conv_backend.rs` `tokio::select!` loop: it
	// subscribes the injected [`ConversationsPush`] (`welcome.received` +
	// `channel.message`) and runs a backstop ticker, driving `ingest_welcome` +
	// the inbound channel poll into the wired [`ConversationsManager`]
	// (`docs/goal/ui/conversations.md` § Receiving into the conversations view,
	// § MLS Welcome at-rest). All MLS crypto stays in [`FaunaMlsBackend`] (§
	// Architectural rules #2); this only routes pushes to it.
	//
	// Spawned on the tokio runtime the `async_runtime = "tokio"` export drives,
	// so the FFI caller (`App.xaml.cs` at login) gets a fire-and-forget start.
	// With no push source injected the loop runs ticker-only (the
	// reconnect/missed-push backstop). The task holds its own per-channel cursor
	// map — independent of [`Self::poll_conversations`]'s, since both only ever
	// dedup against the monotonic channel log.
	StartReceiveLoop()
}

// A FaunaMls-wired conversations session for the native UniFFI clients.
//
// Constructed via [`Self::from_parts`] (plain Rust — takes a `dyn
// ConversationsRpc` for dependency injection, so tests pass a mock nest; the FFI
// factory in `fauna-ffi` passes the real `NestConversationsRpc`). The session
// owns the wired [`ConversationsManager`] (the single observable surface the
// client drives for snapshot/send/rename/…) and the concrete
// [`FaunaMlsBackend`] the receive free functions need by `&` reference.
type ConversationsSession struct {
	ffiObject FfiObject
}

// This session's receive-cycle counters, JSON-encoded (`{"started": N,
// "completed": M, "exit": null | "closed" | "retired" | "panicked"}`) —
// the UniFFI twin of
// [`crate::state_json::conv_receive_cycles_json`] for apps that cross the
// FFI boundary (that function's `Option<&Arc<Self>>` signature isn't
// callable from a `&self` method; this computes the identical shape from
// the same [`ReceiveCycles`] getters — [`Self::receive_cycles`] owns the
// contract, this is not a second count). android's TestAgent re-parses the
// string into its own state object (the established `machine_method_result`
// passthrough shape, `TestAgent.kt::serializeState`) rather than
// re-deriving the counts in Kotlin; tui/linux/web call the free function
// directly and never need this.
func (_self *ConversationsSession) ConvReceiveCyclesJson() string {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsSession")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationssession_conv_receive_cycles_json(
				_pointer, _uniffiStatus),
		}
	}))
}

// UniFFI twin of [`Self::poke_receive_cycle`] — run one receive cycle now
// (convention 14 mechanism 3, `e2e-conventions.md`). Native only, matching
// [`Self::poke_receive_cycle`]'s own gate (wasm never spawns the detached
// receive loop this pokes).
func (_self *ConversationsSession) ConvReceiveNow() {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsSession")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationssession_conv_receive_now(
			_pointer, _uniffiStatus)
		return false
	})
}

// The `data.conversation_threads` e2e state rows, JSON-encoded — the
// UniFFI twin of [`crate::state_json::conversation_threads_json`] for
// apps that cross the FFI boundary (that function takes `&ConversationsManager`
// directly, which a `&self` UniFFI method can supply from
// [`Self::manager`] but a free function outside this crate cannot). Same
// passthrough shape as [`Self::conv_receive_cycles_json`]: android's
// TestAgent re-parses the string into its own state object rather than
// re-deriving the row shape in Kotlin; tui/linux/web call the free
// function (or, for web, the wasm twin) directly and never need this.
func (_self *ConversationsSession) ConversationThreadsJson() string {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsSession")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationssession_conversation_threads_json(
				_pointer, _uniffiStatus),
		}
	}))
}

// Process a same-nest MLS Welcome (`welcome_bytes` for `channel_id_hex`):
// join + bind the group, materialize its thread. Idempotent — a re-delivered
// Welcome for an already-bound channel returns the same thread id without a
// second join. Returns `Some(thread_id)` (the materialized thread's id
// string). Fed by the client's `fauna.conversations.welcome.received` push
// handler. The native twin of the wasm wrapper's `ingestWelcome`; all MLS
// crypto stays in [`FaunaMlsBackend`] (`docs/goal/ui/conversations.md` §
// Architectural rules #2).
func (_self *ConversationsSession) IngestWelcome(channelIdHex string, welcomeBytes []byte) (*string, error) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsSession")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*BackendError](
		FfiConverterBackendErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_conversations_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) *string {
			return FfiConverterOptionalStringINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_conversations_fn_method_conversationssession_ingest_welcome(
			_pointer, FfiConverterStringINSTANCE.Lower(channelIdHex), FfiConverterBytesINSTANCE.Lower(welcomeBytes)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_rust_buffer(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}

// The wired [`ConversationsManager`] — the single observable surface
// (`docs/goal/ui/conversations.md` § State & data shape) the client drives
// for snapshot / send / rename / membership / recipient resolution. The
// FaunaMls backend is already registered on it.
func (_self *ConversationsSession) Manager() *ConversationsManager {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsSession")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterConversationsManagerINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_fauna_conversations_fn_method_conversationssession_manager(
			_pointer, _uniffiStatus)
	}))
}

// This session's `mls_folded_commits` observable, JSON-encoded
// (`{channel_hex: count}`) — the UniFFI twin of
// [`crate::state_json::mls_folded_commits_json`], which this delegates to
// directly ([`Self::backend`] already returns the `&FaunaMlsBackend` it
// wants — no second count, unlike [`Self::conv_receive_cycles_json`]'s
// `Arc<Self>` mismatch). android's TestAgent re-parses the string into its
// own state object rather than re-deriving the counts in Kotlin.
func (_self *ConversationsSession) MlsFoldedCommitsJson() string {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsSession")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationssession_mls_folded_commits_json(
				_pointer, _uniffiStatus),
		}
	}))
}

// The session's retained post-decrypt **local detections**, newest-first — the
// client half of the moderation queue (`docs/goal/behavior/moderation.md`
// § Layout & flow). The queue VM unions these with the server
// `fauna.moderation.actions` rows via `fauna_client_moderation::merge_queue`
// (linux calls it directly; FFI/WASM clients over the shared façade). Empty
// until the receive loop classifies an incoming spam message post-decrypt.
func (_self *ConversationsSession) ModerationLocalDetections() []fauna_client_moderation.LocalDetection {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsSession")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterSequenceLocalDetectionINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationssession_moderation_local_detections(
				_pointer, _uniffiStatus),
		}
	}))
}

// The decrypted plaintext body behind one **local-detection** queue row, by
// its `content_id` (= the classified message's id) — the train-correction
// text source for the client-side tier-1 spam-model write
// (`MailSettingsMachine::train_spam_model_client`; the FFI twin of the
// linux-native `manager().message_body(..)` read). `None` once the message
// has aged out of the thread store — the correction then just clears the
// flag, exactly as before the client write path existed.
func (_self *ConversationsSession) ModerationMessageBody(contentId string) *string {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsSession")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_method_conversationssession_moderation_message_body(
				_pointer, FfiConverterStringINSTANCE.Lower(contentId), _uniffiStatus),
		}
	}))
}

// Drop the local detection for `content_id` after the user trains a correction
// on that queue row (the train button → `fauna.moderation.train` also removes
// the client-side row, since it is corrected). No-op if it is a server row (not
// in this store) or already gone; returns `true` iff one was removed.
func (_self *ConversationsSession) ModerationRemoveLocalDetection(contentId string) bool {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsSession")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_conversations_fn_method_conversationssession_moderation_remove_local_detection(
			_pointer, FfiConverterStringINSTANCE.Lower(contentId), _uniffiStatus)
	}))
}

// Poll every bound FaunaMls channel for new ciphertext (fetch → decode →
// MLS-decrypt → ingest), advancing each channel's `seq` cursor. Returns the
// total number of messages ingested this pass. The reconnect / missed-push
// backstop the client's poll loop ticks — the native twin of the wasm
// wrapper's `pollConversations`.
//
// A per-channel failure is non-fatal — the next tick retries the whole sweep
// — so the loop keeps going and remembers the first error; it only returns
// `Err` when nothing was ingested **and** something failed (otherwise the
// partial success is the useful answer). The per-channel cursor is read +
// copied before the `.await` and written back after, so the `conv_cursors`
// lock is never held across the await.
func (_self *ConversationsSession) PollConversations() (uint32, error) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsSession")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*BackendError](
		FfiConverterBackendErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) C.uint32_t {
			res := C.ffi_fauna_conversations_rust_future_complete_u32(handle, status)
			return res
		},
		// liftFn
		func(ffi C.uint32_t) uint32 {
			return FfiConverterUint32INSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_conversations_fn_method_conversationssession_poll_conversations(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_u32(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_u32(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}

// Poll both mail read-feeds (`INBOX` then `Sent`) for new sealed records
// (fetch → decode → HPKE-decrypt → ingest), advancing each mailbox's
// `after_uid` cursor. Returns the total ingested this pass. The reconnect /
// missed-tick backstop a client may call directly — the mail twin of
// [`Self::poll_conversations`]; [`Self::start_receive_loop`]'s ticker drives
// it automatically. A no-op (`Ok(0)`) until [`Self::register_mail_receive`]
// has wired the sources, or while mail stays unconfigured (the source returns
// an empty page). Same partial-success contract as `poll_conversations`: only
// `Err` when nothing was ingested **and** a mailbox failed. The cursor is
// read + copied before the `.await` and written back after, so the
// `mail_cursors` lock is never held across the await.
func (_self *ConversationsSession) PollMail() (uint32, error) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsSession")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*BackendError](
		FfiConverterBackendErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) C.uint32_t {
			res := C.ffi_fauna_conversations_rust_future_complete_u32(handle, status)
			return res
		},
		// liftFn
		func(ffi C.uint32_t) uint32 {
			return FfiConverterUint32INSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_conversations_fn_method_conversationssession_poll_mail(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_u32(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_u32(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}

// Update the logged-in account's canonical `<handle>@<domain>` — THE one
// self-heal call (`docs/goal/ui/conversations.md` § State & data shape →
// *Self-address: live, never baked*). Both rails read the live cell at use
// time, so the next SMTP send carries this `From:` (a session built before
// identity resolution stops refusing `no_handle`), the FaunaMls data plane
// routes same-nest peers against this domain, and the reply-all self-drop
// compares against this address. Call it from wherever identity state
// lands: login-time resolution, the background identity refresh, a
// server-side handle rename. Idempotent; nothing is rebuilt.
func (_self *ConversationsSession) SetSelfAddress(selfAddress string) {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsSession")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_conversationssession_set_self_address(
			_pointer, FfiConverterStringINSTANCE.Lower(selfAddress), _uniffiStatus)
		return false
	})
}

// Start the detached push-driven receive loop and return immediately. The
// native twin of the linux `conv_backend.rs` `tokio::select!` loop: it
// subscribes the injected [`ConversationsPush`] (`welcome.received` +
// `channel.message`) and runs a backstop ticker, driving `ingest_welcome` +
// the inbound channel poll into the wired [`ConversationsManager`]
// (`docs/goal/ui/conversations.md` § Receiving into the conversations view,
// § MLS Welcome at-rest). All MLS crypto stays in [`FaunaMlsBackend`] (§
// Architectural rules #2); this only routes pushes to it.
//
// Spawned on the tokio runtime the `async_runtime = "tokio"` export drives,
// so the FFI caller (`App.xaml.cs` at login) gets a fire-and-forget start.
// With no push source injected the loop runs ticker-only (the
// reconnect/missed-push backstop). The task holds its own per-channel cursor
// map — independent of [`Self::poll_conversations`]'s, since both only ever
// dedup against the monotonic channel log.
func (_self *ConversationsSession) StartReceiveLoop() {
	_pointer := _self.ffiObject.incrementPointer("*ConversationsSession")
	defer _self.ffiObject.decrementPointer()
	uniffiRustCallAsync[error](
		nil,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_fauna_conversations_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_fauna_conversations_fn_method_conversationssession_start_receive_loop(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_conversations_rust_future_free_void(handle)
		},
	)

}
func (object *ConversationsSession) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterConversationsSession struct{}

var FfiConverterConversationsSessionINSTANCE = FfiConverterConversationsSession{}

func (c FfiConverterConversationsSession) Lift(handle C.uint64_t) *ConversationsSession {
	result := &ConversationsSession{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_fauna_conversations_fn_clone_conversationssession(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_fauna_conversations_fn_free_conversationssession(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*ConversationsSession).Destroy)
	return result
}

func (c FfiConverterConversationsSession) Read(reader io.Reader) *ConversationsSession {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterConversationsSession) Lower(value *ConversationsSession) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*ConversationsSession")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterConversationsSession) Write(writer io.Writer, value *ConversationsSession) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalConversationsSession(handle uint64) *ConversationsSession {
	return FfiConverterConversationsSessionINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalConversationsSession(value *ConversationsSession) uint64 {
	return uint64(FfiConverterConversationsSessionINSTANCE.Lower(value))
}

type FfiDestroyerConversationsSession struct{}

func (_ FfiDestroyerConversationsSession) Destroy(value *ConversationsSession) {
	value.Destroy()
}

type SnapshotObserver interface {
	// Called whenever the manager's observable state changes. The
	// observer reads fresh snapshots via `ConversationsManager::snapshot()`.
	OnChanged()
}
type SnapshotObserverImpl struct {
	ffiObject FfiObject
}

// Called whenever the manager's observable state changes. The
// observer reads fresh snapshots via `ConversationsManager::snapshot()`.
func (_self *SnapshotObserverImpl) OnChanged() {
	_pointer := _self.ffiObject.incrementPointer("SnapshotObserver")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_fauna_conversations_fn_method_snapshotobserver_on_changed(
			_pointer, _uniffiStatus)
		return false
	})
}
func (object *SnapshotObserverImpl) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterSnapshotObserver struct {
	handleMap *concurrentHandleMap[SnapshotObserver]
}

var FfiConverterSnapshotObserverINSTANCE = FfiConverterSnapshotObserver{
	handleMap: newConcurrentHandleMap[SnapshotObserver](),
}

func (c FfiConverterSnapshotObserver) Lift(handle C.uint64_t) SnapshotObserver {
	if uint64(handle)&1 == 0 {
		// Rust-generated handle (even), construct a new object wrapping the handle
		result := &SnapshotObserverImpl{
			newFfiObject(
				handle,
				func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
					return C.uniffi_fauna_conversations_fn_clone_snapshotobserver(handle, status)
				},
				func(handle C.uint64_t, status *C.RustCallStatus) {
					C.uniffi_fauna_conversations_fn_free_snapshotobserver(handle, status)
				},
			),
		}
		runtime.SetFinalizer(result, (*SnapshotObserverImpl).Destroy)
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

func (c FfiConverterSnapshotObserver) Read(reader io.Reader) SnapshotObserver {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterSnapshotObserver) Lower(value SnapshotObserver) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	if val, ok := value.(*SnapshotObserverImpl); ok {
		// Rust-backed object, clone the handle
		handle := val.ffiObject.incrementPointer("SnapshotObserver")
		defer val.ffiObject.decrementPointer()
		return handle
	} else {
		// Go-backed object, insert into handle map
		return C.uint64_t(c.handleMap.insert(value))
	}
}

func (c FfiConverterSnapshotObserver) Write(writer io.Writer, value SnapshotObserver) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalSnapshotObserver(handle uint64) SnapshotObserver {
	return FfiConverterSnapshotObserverINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalSnapshotObserver(value SnapshotObserver) uint64 {
	return uint64(FfiConverterSnapshotObserverINSTANCE.Lower(value))
}

type FfiDestroyerSnapshotObserver struct{}

func (_ FfiDestroyerSnapshotObserver) Destroy(value SnapshotObserver) {
	if val, ok := value.(*SnapshotObserverImpl); ok {
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

//export fauna_conversations_observer_cgo_dispatchCallbackInterfaceSnapshotObserverMethod0
func fauna_conversations_observer_cgo_dispatchCallbackInterfaceSnapshotObserverMethod0(uniffiHandle C.uint64_t, uniffiOutReturn *C.void, callStatus *C.RustCallStatus) {
	handle := uint64(uniffiHandle)
	uniffiObj, ok := FfiConverterSnapshotObserverINSTANCE.handleMap.tryGet(handle)
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}

	uniffiObj.OnChanged()

}

var UniffiVTableCallbackInterfaceSnapshotObserverINSTANCE = C.UniffiVTableCallbackInterfaceSnapshotObserver{
	uniffiFree:  (C.UniffiCallbackInterfaceFree)(C.fauna_conversations_observer_cgo_dispatchCallbackInterfaceSnapshotObserverFree),
	uniffiClone: (C.UniffiCallbackInterfaceClone)(C.fauna_conversations_observer_cgo_dispatchCallbackInterfaceSnapshotObserverClone),
	onChanged:   (C.UniffiCallbackInterfaceSnapshotObserverMethod0)(C.fauna_conversations_observer_cgo_dispatchCallbackInterfaceSnapshotObserverMethod0),
}

//export fauna_conversations_observer_cgo_dispatchCallbackInterfaceSnapshotObserverFree
func fauna_conversations_observer_cgo_dispatchCallbackInterfaceSnapshotObserverFree(handle C.uint64_t) {
	FfiConverterSnapshotObserverINSTANCE.handleMap.remove(uint64(handle))
}

//export fauna_conversations_observer_cgo_dispatchCallbackInterfaceSnapshotObserverClone
func fauna_conversations_observer_cgo_dispatchCallbackInterfaceSnapshotObserverClone(handle C.uint64_t) C.uint64_t {
	val, ok := FfiConverterSnapshotObserverINSTANCE.handleMap.tryGet(uint64(handle))
	if !ok {
		panic(fmt.Errorf("no callback in handle map: %d", handle))
	}
	return C.uint64_t(FfiConverterSnapshotObserverINSTANCE.handleMap.insert(val))
}

func (c FfiConverterSnapshotObserver) register() {
	C.uniffi_fauna_conversations_fn_init_callback_vtable_snapshotobserver(&UniffiVTableCallbackInterfaceSnapshotObserverINSTANCE)
}

type AddParticipantState struct {
	TargetThreadId ThreadId
	Picker         RecipientPickerState
	// Does confirming this overlay reach the wire? `true` exactly when the
	// target is a bound FaunaMls **group** — the one `(rail, flavor)` that
	// adds *in place*, opening the add commit by fetching the newcomer's key
	// package. Every other pairing forks or edits the snapshot only: a
	// FaunaMls 1:1 forks a new group whose first *send* bootstraps it, and a
	// non-FaunaMls rail has no wire membership op at all.
	//
	// **Carried for the offline gate, and that is the whole reason it
	// exists.** The two arms are "needs a nest" and "issues nothing", which
	// is a class difference no undiscriminated state could state — so a
	// client gating `add-participant-confirm` on
	// `fauna.conversations.keypackage.fetch` unconditionally would grey a
	// fork that works perfectly offline, exactly the over-claim
	// `../../../docs/goal/architecture/account-data-plane.md`
	// § The offline-mutation contract → *How a surface asks* forbids. Every
	// app inherits the one discriminant instead of re-deriving it: tui reads
	// it straight into `Action::ConfirmAddParticipant { in_place_mls_group }`,
	// and the UniFFI apps read the same field off the snapshot (priority #2 —
	// the rail/flavor test is shared logic, not per-app glue).
	//
	// **Not the authority for the wire op.** [`ConversationsManager::
	// confirm_add_participant`] re-derives the same test from the live thread
	// and acts on *that*, so a snapshot a client held too long can never cause
	// a wrong commit — the field only decides what the paint offers. The two
	// agree by construction: a thread's `rail`/`flavor` are set once, when the
	// store builds it (`store/threads.rs`), and are never reassigned, so they
	// cannot drift while an overlay is open.
	InPlaceMlsGroup bool
}

func (r *AddParticipantState) Destroy() {
	FfiDestroyerTypeThreadId{}.Destroy(r.TargetThreadId)
	FfiDestroyerRecipientPickerState{}.Destroy(r.Picker)
	FfiDestroyerBool{}.Destroy(r.InPlaceMlsGroup)
}

type FfiConverterAddParticipantState struct{}

var FfiConverterAddParticipantStateINSTANCE = FfiConverterAddParticipantState{}

func (c FfiConverterAddParticipantState) Lift(rb RustBufferI) AddParticipantState {
	return LiftFromRustBuffer[AddParticipantState](c, rb)
}

func (c FfiConverterAddParticipantState) Read(reader io.Reader) AddParticipantState {
	return AddParticipantState{
		FfiConverterTypeThreadIdINSTANCE.Read(reader),
		FfiConverterRecipientPickerStateINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterAddParticipantState) Lower(value AddParticipantState) C.RustBuffer {
	return LowerIntoRustBuffer[AddParticipantState](c, value)
}

func (c FfiConverterAddParticipantState) LowerExternal(value AddParticipantState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AddParticipantState](c, value))
}

func (c FfiConverterAddParticipantState) Write(writer io.Writer, value AddParticipantState) {
	FfiConverterTypeThreadIdINSTANCE.Write(writer, value.TargetThreadId)
	FfiConverterRecipientPickerStateINSTANCE.Write(writer, value.Picker)
	FfiConverterBoolINSTANCE.Write(writer, value.InPlaceMlsGroup)
}

type FfiDestroyerAddParticipantState struct{}

func (_ FfiDestroyerAddParticipantState) Destroy(value AddParticipantState) {
	value.Destroy()
}

// A staged compose attachment (`attachment-button` → `add_attachment`). Light
// by design — the **bytes never live in the snapshot**: `add_attachment` hashes
// the picked file's bytes, caches them under `blob_hash` in the manager's
// attachment store, and stages only this metadata, so an observed
// `ComposeState` stays cheap to diff over UniFFI even with a multi-MB image
// staged. `send` re-resolves the bytes from the store by `blob_hash`
// (`docs/goal/ui/conversations.md` § Attachments).
type AttachmentDraft struct {
	BlobHash  string
	Filename  string
	MimeType  string
	SizeBytes uint64
	IsImage   bool
}

func (r *AttachmentDraft) Destroy() {
	FfiDestroyerString{}.Destroy(r.BlobHash)
	FfiDestroyerString{}.Destroy(r.Filename)
	FfiDestroyerString{}.Destroy(r.MimeType)
	FfiDestroyerUint64{}.Destroy(r.SizeBytes)
	FfiDestroyerBool{}.Destroy(r.IsImage)
}

type FfiConverterAttachmentDraft struct{}

var FfiConverterAttachmentDraftINSTANCE = FfiConverterAttachmentDraft{}

func (c FfiConverterAttachmentDraft) Lift(rb RustBufferI) AttachmentDraft {
	return LiftFromRustBuffer[AttachmentDraft](c, rb)
}

func (c FfiConverterAttachmentDraft) Read(reader io.Reader) AttachmentDraft {
	return AttachmentDraft{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterAttachmentDraft) Lower(value AttachmentDraft) C.RustBuffer {
	return LowerIntoRustBuffer[AttachmentDraft](c, value)
}

func (c FfiConverterAttachmentDraft) LowerExternal(value AttachmentDraft) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AttachmentDraft](c, value))
}

func (c FfiConverterAttachmentDraft) Write(writer io.Writer, value AttachmentDraft) {
	FfiConverterStringINSTANCE.Write(writer, value.BlobHash)
	FfiConverterStringINSTANCE.Write(writer, value.Filename)
	FfiConverterStringINSTANCE.Write(writer, value.MimeType)
	FfiConverterUint64INSTANCE.Write(writer, value.SizeBytes)
	FfiConverterBoolINSTANCE.Write(writer, value.IsImage)
}

type FfiDestroyerAttachmentDraft struct{}

func (_ FfiDestroyerAttachmentDraft) Destroy(value AttachmentDraft) {
	value.Destroy()
}

// A rendered attachment on a received/sent message. The unified shape every
// app renders off (`docs/goal/ui/conversations.md` § Attachments):
// `dm-attachment-image[i]` for `is_image`, else `dm-attachment-file[i]`.
//
// `blob_hash` is the content handle — lowercase-hex BLAKE3 of the attachment's
// plaintext bytes. A client resolves it to the real bytes through the shared
// loader: [`crate::ConversationsManager::attachment_bytes`] (the in-memory
// cache the inbound parse / send echo populated for nest-backed rails), and —
// for FaunaMls — the nest `__conv` blob store GET + `decrypt_blob` (follow-on).
// This replaces the old `uri` (a per-app cache path), so the *handle* is
// uniform and *resolution* is the only per-rail / client-glue concern.
//
// `c2pa` is the per-attachment C2PA-signed verdict, probed at receive time
// from the decrypted bytes (`fauna_media::process::detect_c2pa`,
// `attachments_to_inbound`). Real on every native app (FFI included —
// `c2pa-detect` has shipped there since 2026-06-30) and a genuine `false`
// stub only on web (wasm never enables `c2pa-detect`, to keep the heavy
// `c2pa` tree out of the bundle) — `docs/goal/ui/conversations.md` §
// Attachments "C2PA on-device".
type AttachmentSnapshot struct {
	BlobHash  string
	Filename  string
	MimeType  string
	SizeBytes uint64
	IsImage   bool
	C2pa      bool
}

func (r *AttachmentSnapshot) Destroy() {
	FfiDestroyerString{}.Destroy(r.BlobHash)
	FfiDestroyerString{}.Destroy(r.Filename)
	FfiDestroyerString{}.Destroy(r.MimeType)
	FfiDestroyerUint64{}.Destroy(r.SizeBytes)
	FfiDestroyerBool{}.Destroy(r.IsImage)
	FfiDestroyerBool{}.Destroy(r.C2pa)
}

type FfiConverterAttachmentSnapshot struct{}

var FfiConverterAttachmentSnapshotINSTANCE = FfiConverterAttachmentSnapshot{}

func (c FfiConverterAttachmentSnapshot) Lift(rb RustBufferI) AttachmentSnapshot {
	return LiftFromRustBuffer[AttachmentSnapshot](c, rb)
}

func (c FfiConverterAttachmentSnapshot) Read(reader io.Reader) AttachmentSnapshot {
	return AttachmentSnapshot{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterAttachmentSnapshot) Lower(value AttachmentSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[AttachmentSnapshot](c, value)
}

func (c FfiConverterAttachmentSnapshot) LowerExternal(value AttachmentSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AttachmentSnapshot](c, value))
}

func (c FfiConverterAttachmentSnapshot) Write(writer io.Writer, value AttachmentSnapshot) {
	FfiConverterStringINSTANCE.Write(writer, value.BlobHash)
	FfiConverterStringINSTANCE.Write(writer, value.Filename)
	FfiConverterStringINSTANCE.Write(writer, value.MimeType)
	FfiConverterUint64INSTANCE.Write(writer, value.SizeBytes)
	FfiConverterBoolINSTANCE.Write(writer, value.IsImage)
	FfiConverterBoolINSTANCE.Write(writer, value.C2pa)
}

type FfiDestroyerAttachmentSnapshot struct{}

func (_ FfiDestroyerAttachmentSnapshot) Destroy(value AttachmentSnapshot) {
	value.Destroy()
}

type ComposeState struct {
	BodyDraft    string
	SubjectDraft *string
	Attachments  []AttachmentDraft
	ReplyTo      *MessageId
	// The To/Cc of the reply about to be sent — an editable per-reply draft,
	// seeded by `dm-reply-button` (sender-only) or `dm-reply-all-button`
	// (every thread participant but self), then editable via the always-visible
	// "To" line (`conversations.md` § Participants vs reply recipients).
	// Populated only on rails whose `ThreadCapabilities.supports_recipient_selection`
	// is true (mail); empty elsewhere — and when empty the SMTP backend falls
	// back to the historical participants-minus-self, so a plain send (no reply
	// seed) still addresses the thread. Removing a chip drops a recipient from
	// *this reply only* — thread history is untouched.
	ReplyRecipients []TypedAddress
	RecipientPicker *RecipientPickerState
	SendState       SendState
	// The list-send view when this compose's one mail recipient is one of the
	// account's own mailing lists (`mail-mass-mailing.md` § Composing a list
	// message) — `None` for every other compose. Derived by
	// [`crate::ConversationsManager::refresh_list_send`] from the nest's
	// figures, so it never rests with the draft (`store::drafts::persistable`
	// writes it `None`): a restored draft re-derives.
	// Defaulted for UniFFI so the shells' positional test constructors stay
	// valid as the record grows.
	ListSend *ListSendView
}

func (r *ComposeState) Destroy() {
	FfiDestroyerString{}.Destroy(r.BodyDraft)
	FfiDestroyerOptionalString{}.Destroy(r.SubjectDraft)
	FfiDestroyerSequenceAttachmentDraft{}.Destroy(r.Attachments)
	FfiDestroyerOptionalTypeMessageId{}.Destroy(r.ReplyTo)
	FfiDestroyerSequenceTypedAddress{}.Destroy(r.ReplyRecipients)
	FfiDestroyerOptionalRecipientPickerState{}.Destroy(r.RecipientPicker)
	FfiDestroyerSendState{}.Destroy(r.SendState)
	FfiDestroyerOptionalListSendView{}.Destroy(r.ListSend)
}

type FfiConverterComposeState struct{}

var FfiConverterComposeStateINSTANCE = FfiConverterComposeState{}

func (c FfiConverterComposeState) Lift(rb RustBufferI) ComposeState {
	return LiftFromRustBuffer[ComposeState](c, rb)
}

func (c FfiConverterComposeState) Read(reader io.Reader) ComposeState {
	return ComposeState{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterSequenceAttachmentDraftINSTANCE.Read(reader),
		FfiConverterOptionalTypeMessageIdINSTANCE.Read(reader),
		FfiConverterSequenceTypedAddressINSTANCE.Read(reader),
		FfiConverterOptionalRecipientPickerStateINSTANCE.Read(reader),
		FfiConverterSendStateINSTANCE.Read(reader),
		FfiConverterOptionalListSendViewINSTANCE.Read(reader),
	}
}

func (c FfiConverterComposeState) Lower(value ComposeState) C.RustBuffer {
	return LowerIntoRustBuffer[ComposeState](c, value)
}

func (c FfiConverterComposeState) LowerExternal(value ComposeState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ComposeState](c, value))
}

func (c FfiConverterComposeState) Write(writer io.Writer, value ComposeState) {
	FfiConverterStringINSTANCE.Write(writer, value.BodyDraft)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.SubjectDraft)
	FfiConverterSequenceAttachmentDraftINSTANCE.Write(writer, value.Attachments)
	FfiConverterOptionalTypeMessageIdINSTANCE.Write(writer, value.ReplyTo)
	FfiConverterSequenceTypedAddressINSTANCE.Write(writer, value.ReplyRecipients)
	FfiConverterOptionalRecipientPickerStateINSTANCE.Write(writer, value.RecipientPicker)
	FfiConverterSendStateINSTANCE.Write(writer, value.SendState)
	FfiConverterOptionalListSendViewINSTANCE.Write(writer, value.ListSend)
}

type FfiDestroyerComposeState struct{}

func (_ FfiDestroyerComposeState) Destroy(value ComposeState) {
	value.Destroy()
}

type ConversationsSnapshot struct {
	Threads          []ThreadSummary
	Sort             SortOrder
	SearchQuery      *string
	SelectedThreadId *ThreadId
	NewThreadCompose *ComposeState
	AddParticipant   *AddParticipantState
	// Page-level error → `error-message` (`conversations.md` § Errors & edge
	// cases), the same shape and role as `FeedSnapshot::error`.
	//
	// Carries the failures of the **membership/label wire ops** —
	// `confirm_add_participant`, `remove_participant`, `rename_thread` — each
	// of which mutates the snapshot optimistically and then fires a wire op
	// that can fail. Before this field they only `tracing::warn!`d, so a failed
	// add closed the overlay and did nothing visible: a dropped command by
	// `../architecture/testing.md` point 11's definition, and the inverse of
	// the "rendered truthfully as not-yet-shared" property
	// `mls-group-key-material.md` § M2 requires of a half-added member.
	//
	// **Not** the compose send path: a send failure is compose-scoped and
	// already surfaced through `ComposeState::send_state`
	// (`SendState::Failed { reason }`, whose `reason` is a `LocalizedText` too —
	// one element, one carrier), which clients render from the *active* compose.
	// Two truths for one gesture would be the drift; each gesture has exactly
	// one. A new gesture supersedes the previous one's error — every
	// producer clears this on entry, `send`/`send_new_thread` included.
	Error *fauna_core.LocalizedText
	// The **launch floor** — where this run's news starts, in message-stamp
	// terms: the moment the thread store was created, or wiped for an identity
	// change (`conversations.md` § State & data shape → *When a thread is
	// read*). A message stamped before it is history, whichever snapshot
	// delivers it — it never counts as unread, and the new-message banner
	// decision (`MessageNotificationTracker::diff`, § Where logic lives) reads
	// this same value so a thread a slower rail delivers after the seed never
	// banners for old mail. Published rather than re-read by each app so the
	// two decisions can never disagree about where news starts.
	LaunchFloorMs int64
	// Every community-room invitation standing for this account, verified —
	// `room-invitation[i]` atop the conversation list. Refreshed on the
	// receive loop's sweep; an accepted or declined one leaves the list in the
	// same act (`conversation-rooms.md` § Join rules and invites).
	RoomInvitations []RoomInvitationSnapshot
	// The bridges serving this account, by declared identity and ordered by
	// label — what the recipient picker names, so the user can see which far
	// networks a typed address may reach, and what labels a resolved bridged
	// address (`conversations.md` § Where logic lives → *The `Bridged`
	// adapter*, ruling 2 (d): the grammar is the nest's, so the picker lists
	// bridges by label and never matches an address itself). Empty with no
	// bridge consented.
	Bridges []fauna_core.BridgeIdentitySnapshot
}

func (r *ConversationsSnapshot) Destroy() {
	FfiDestroyerSequenceThreadSummary{}.Destroy(r.Threads)
	FfiDestroyerSortOrder{}.Destroy(r.Sort)
	FfiDestroyerOptionalString{}.Destroy(r.SearchQuery)
	FfiDestroyerOptionalTypeThreadId{}.Destroy(r.SelectedThreadId)
	FfiDestroyerOptionalComposeState{}.Destroy(r.NewThreadCompose)
	FfiDestroyerOptionalAddParticipantState{}.Destroy(r.AddParticipant)
	FfiDestroyerOptionalLocalizedText{}.Destroy(r.Error)
	FfiDestroyerInt64{}.Destroy(r.LaunchFloorMs)
	FfiDestroyerSequenceRoomInvitationSnapshot{}.Destroy(r.RoomInvitations)
	FfiDestroyerSequenceBridgeIdentitySnapshot{}.Destroy(r.Bridges)
}

type FfiConverterConversationsSnapshot struct{}

var FfiConverterConversationsSnapshotINSTANCE = FfiConverterConversationsSnapshot{}

func (c FfiConverterConversationsSnapshot) Lift(rb RustBufferI) ConversationsSnapshot {
	return LiftFromRustBuffer[ConversationsSnapshot](c, rb)
}

func (c FfiConverterConversationsSnapshot) Read(reader io.Reader) ConversationsSnapshot {
	return ConversationsSnapshot{
		FfiConverterSequenceThreadSummaryINSTANCE.Read(reader),
		FfiConverterSortOrderINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalTypeThreadIdINSTANCE.Read(reader),
		FfiConverterOptionalComposeStateINSTANCE.Read(reader),
		FfiConverterOptionalAddParticipantStateINSTANCE.Read(reader),
		FfiConverterOptionalLocalizedTextINSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterSequenceRoomInvitationSnapshotINSTANCE.Read(reader),
		FfiConverterSequenceBridgeIdentitySnapshotINSTANCE.Read(reader),
	}
}

func (c FfiConverterConversationsSnapshot) Lower(value ConversationsSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[ConversationsSnapshot](c, value)
}

func (c FfiConverterConversationsSnapshot) LowerExternal(value ConversationsSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ConversationsSnapshot](c, value))
}

func (c FfiConverterConversationsSnapshot) Write(writer io.Writer, value ConversationsSnapshot) {
	FfiConverterSequenceThreadSummaryINSTANCE.Write(writer, value.Threads)
	FfiConverterSortOrderINSTANCE.Write(writer, value.Sort)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.SearchQuery)
	FfiConverterOptionalTypeThreadIdINSTANCE.Write(writer, value.SelectedThreadId)
	FfiConverterOptionalComposeStateINSTANCE.Write(writer, value.NewThreadCompose)
	FfiConverterOptionalAddParticipantStateINSTANCE.Write(writer, value.AddParticipant)
	FfiConverterOptionalLocalizedTextINSTANCE.Write(writer, value.Error)
	FfiConverterInt64INSTANCE.Write(writer, value.LaunchFloorMs)
	FfiConverterSequenceRoomInvitationSnapshotINSTANCE.Write(writer, value.RoomInvitations)
	FfiConverterSequenceBridgeIdentitySnapshotINSTANCE.Write(writer, value.Bridges)
}

type FfiDestroyerConversationsSnapshot struct{}

func (_ FfiDestroyerConversationsSnapshot) Destroy(value ConversationsSnapshot) {
	value.Destroy()
}

// What a cross-group eviction actually achieved, per group.
//
// Deliberately **not** a boolean. A partial eviction is the ordinary outcome
// under a flaky link — removing someone from 3 of their 5 groups — and the
// whole honesty of the review surface turns on not rounding that to success.
type CrossGroupEviction struct {
	// Groups the person was in and is no longer, this call.
	Evicted []ThreadId
	// Groups the person is still in because the removal failed, with the
	// backend's reason. A retry targets exactly these, for free — see
	// [`Self::earned_verdict`].
	Failed []EvictionFailure
	// Seats this driver cannot clear from here at all — § Propagation rule
	// (5)'s typed, rendered fact (`identity-succession.md`): a raised seat
	// never escapes silently and never lets `Removed` be earned while it
	// stands. Unlike [`Self::failed`] these are not retryable *here*; each
	// class names the room its remedy lives in, and either remedy converges —
	// once taken, the seat is out of the raised span at the next press.
	Unreachable []UnreachableSeat
}

func (r *CrossGroupEviction) Destroy() {
	FfiDestroyerSequenceTypeThreadId{}.Destroy(r.Evicted)
	FfiDestroyerSequenceEvictionFailure{}.Destroy(r.Failed)
	FfiDestroyerSequenceUnreachableSeat{}.Destroy(r.Unreachable)
}

type FfiConverterCrossGroupEviction struct{}

var FfiConverterCrossGroupEvictionINSTANCE = FfiConverterCrossGroupEviction{}

func (c FfiConverterCrossGroupEviction) Lift(rb RustBufferI) CrossGroupEviction {
	return LiftFromRustBuffer[CrossGroupEviction](c, rb)
}

func (c FfiConverterCrossGroupEviction) Read(reader io.Reader) CrossGroupEviction {
	return CrossGroupEviction{
		FfiConverterSequenceTypeThreadIdINSTANCE.Read(reader),
		FfiConverterSequenceEvictionFailureINSTANCE.Read(reader),
		FfiConverterSequenceUnreachableSeatINSTANCE.Read(reader),
	}
}

func (c FfiConverterCrossGroupEviction) Lower(value CrossGroupEviction) C.RustBuffer {
	return LowerIntoRustBuffer[CrossGroupEviction](c, value)
}

func (c FfiConverterCrossGroupEviction) LowerExternal(value CrossGroupEviction) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[CrossGroupEviction](c, value))
}

func (c FfiConverterCrossGroupEviction) Write(writer io.Writer, value CrossGroupEviction) {
	FfiConverterSequenceTypeThreadIdINSTANCE.Write(writer, value.Evicted)
	FfiConverterSequenceEvictionFailureINSTANCE.Write(writer, value.Failed)
	FfiConverterSequenceUnreachableSeatINSTANCE.Write(writer, value.Unreachable)
}

type FfiDestroyerCrossGroupEviction struct{}

func (_ FfiDestroyerCrossGroupEviction) Destroy(value CrossGroupEviction) {
	value.Destroy()
}

// One group a [`CrossGroupEviction`] could not clear, with the backend's
// reason. A named record rather than a `(ThreadId, String)` tuple — UniFFI
// has no `Lower`/`Lift` impl for a bare tuple, only for records.
type EvictionFailure struct {
	Thread ThreadId
	Reason string
}

func (r *EvictionFailure) Destroy() {
	FfiDestroyerTypeThreadId{}.Destroy(r.Thread)
	FfiDestroyerString{}.Destroy(r.Reason)
}

type FfiConverterEvictionFailure struct{}

var FfiConverterEvictionFailureINSTANCE = FfiConverterEvictionFailure{}

func (c FfiConverterEvictionFailure) Lift(rb RustBufferI) EvictionFailure {
	return LiftFromRustBuffer[EvictionFailure](c, rb)
}

func (c FfiConverterEvictionFailure) Read(reader io.Reader) EvictionFailure {
	return EvictionFailure{
		FfiConverterTypeThreadIdINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterEvictionFailure) Lower(value EvictionFailure) C.RustBuffer {
	return LowerIntoRustBuffer[EvictionFailure](c, value)
}

func (c FfiConverterEvictionFailure) LowerExternal(value EvictionFailure) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[EvictionFailure](c, value))
}

func (c FfiConverterEvictionFailure) Write(writer io.Writer, value EvictionFailure) {
	FfiConverterTypeThreadIdINSTANCE.Write(writer, value.Thread)
	FfiConverterStringINSTANCE.Write(writer, value.Reason)
}

type FfiDestroyerEvictionFailure struct{}

func (_ FfiDestroyerEvictionFailure) Destroy(value EvictionFailure) {
	value.Destroy()
}

// What the compose form shows for a compose addressed to one of the
// account's own lists. `None` on [`crate::compose::ComposeState::list_send`]
// for every other compose.
type ListSendView struct {
	// The list's name, as the texts below name it.
	ListName string
	// Subscribed recipients the next send reaches.
	MemberCount uint64
	// `dm-compose-list-send-warning`.
	SendWarning fauna_core.LocalizedText
	// `dm-compose-list-quota-warning` — shown only when today's remaining
	// allowance is within 10% of the cap, or too small for this list.
	QuotaWarning *fauna_core.LocalizedText
	// `dm-compose-list-send-progress` — shown once the list has a send.
	Progress *fauna_core.LocalizedText
}

func (r *ListSendView) Destroy() {
	FfiDestroyerString{}.Destroy(r.ListName)
	FfiDestroyerUint64{}.Destroy(r.MemberCount)
	fauna_core.FfiDestroyerLocalizedText{}.Destroy(r.SendWarning)
	FfiDestroyerOptionalLocalizedText{}.Destroy(r.QuotaWarning)
	FfiDestroyerOptionalLocalizedText{}.Destroy(r.Progress)
}

type FfiConverterListSendView struct{}

var FfiConverterListSendViewINSTANCE = FfiConverterListSendView{}

func (c FfiConverterListSendView) Lift(rb RustBufferI) ListSendView {
	return LiftFromRustBuffer[ListSendView](c, rb)
}

func (c FfiConverterListSendView) Read(reader io.Reader) ListSendView {
	return ListSendView{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
		fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader),
		FfiConverterOptionalLocalizedTextINSTANCE.Read(reader),
		FfiConverterOptionalLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterListSendView) Lower(value ListSendView) C.RustBuffer {
	return LowerIntoRustBuffer[ListSendView](c, value)
}

func (c FfiConverterListSendView) LowerExternal(value ListSendView) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ListSendView](c, value))
}

func (c FfiConverterListSendView) Write(writer io.Writer, value ListSendView) {
	FfiConverterStringINSTANCE.Write(writer, value.ListName)
	FfiConverterUint64INSTANCE.Write(writer, value.MemberCount)
	fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, value.SendWarning)
	FfiConverterOptionalLocalizedTextINSTANCE.Write(writer, value.QuotaWarning)
	FfiConverterOptionalLocalizedTextINSTANCE.Write(writer, value.Progress)
}

type FfiDestroyerListSendView struct{}

func (_ FfiDestroyerListSendView) Destroy(value ListSendView) {
	value.Destroy()
}

// Whole-message crypto badges. Content credentials are not one of them: a
// C2PA verdict belongs to the bytes it was probed over, so it rides each
// attachment ([`AttachmentSnapshot::c2pa`]), never the message.
type MessageBadges struct {
	Encrypted      bool
	Signed         bool
	Verified       bool
	ContentWarning *string
}

func (r *MessageBadges) Destroy() {
	FfiDestroyerBool{}.Destroy(r.Encrypted)
	FfiDestroyerBool{}.Destroy(r.Signed)
	FfiDestroyerBool{}.Destroy(r.Verified)
	FfiDestroyerOptionalString{}.Destroy(r.ContentWarning)
}

type FfiConverterMessageBadges struct{}

var FfiConverterMessageBadgesINSTANCE = FfiConverterMessageBadges{}

func (c FfiConverterMessageBadges) Lift(rb RustBufferI) MessageBadges {
	return LiftFromRustBuffer[MessageBadges](c, rb)
}

func (c FfiConverterMessageBadges) Read(reader io.Reader) MessageBadges {
	return MessageBadges{
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterMessageBadges) Lower(value MessageBadges) C.RustBuffer {
	return LowerIntoRustBuffer[MessageBadges](c, value)
}

func (c FfiConverterMessageBadges) LowerExternal(value MessageBadges) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[MessageBadges](c, value))
}

func (c FfiConverterMessageBadges) Write(writer io.Writer, value MessageBadges) {
	FfiConverterBoolINSTANCE.Write(writer, value.Encrypted)
	FfiConverterBoolINSTANCE.Write(writer, value.Signed)
	FfiConverterBoolINSTANCE.Write(writer, value.Verified)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.ContentWarning)
}

type FfiDestroyerMessageBadges struct{}

func (_ FfiDestroyerMessageBadges) Destroy(value MessageBadges) {
	value.Destroy()
}

type MessageSnapshot struct {
	MessageId     MessageId
	Sender        TypedAddress
	SenderDisplay string
	// The raw body source (markdown / plaintext / already-converted inbound
	// HTML). **Retained as the canonical text source**, not a deletable render
	// sibling: [`document`](Self::document) below is its render projection, and
	// the thread-list snippet ([`crate::store::threads`] `summarize` →
	// `fauna_core::markdown::markdown_to_plaintext(&body)`) reads it — the exact
	// analogue of feed's retained `fauna_feed::PostSummary::body` (which feeds
	// the quoted-post projection). No client re-parses `body` at render time —
	// every shell walks `document` (render-model.md § D1). The one residual is a
	// priority-#1 e2e drift: the `dm-message-text` automation read exposes the
	// painted *document* text on linux/web/android/windows but the raw `body`
	// on apple — unified by swapping apple's `automationValue` closures to a
	// document-derived plaintext (`RenderDocument::to_plaintext`), not by
	// deleting this field.
	Body string
	// The structured, semantic render document for this message — the one
	// representation every app paints (render-model.md § D1/D2). Produced
	// once by the manager via [`document_for_message`]: the text body (the
	// `body_format` discriminant is consumed *inside* the producer) plus one
	// [`RenderBlock::Attachment`] block per attachment folded in body order. No
	// client re-parses the body or reads a sibling `body_format` / `attachments`
	// field at render time — the document is the complete tree.
	Document    fauna_core.RenderDocument
	TimestampMs int64
	SubjectLine *string
	Badges      MessageBadges
	ReplyTo     *MessageId
	// Aggregated reactions on this message, ordered by first-add appearance.
	// Empty until the manager folds reaction events in (task B3+).
	Reactions []ReactionGroup
	// Whether this message has been deleted. `false` until the manager
	// processes a matching `ChannelMessageBody::Delete` event (task B3+).
	Deleted bool
	// Whether the local actor sent this message. Used to gate sender-only
	// delete (task B3+). Set at snapshot construction time.
	IsOwn bool
	// `Some(reference)` iff this message was taken down under a legal obligation
	// (`moderation.md` § Categories & enforcement item 1 — the conversation twin
	// of the post tombstone / `fauna_feed::QuotedPostView::legal_takedown_ref`).
	// The nest **withheld** the sealed envelope, so [`document`](Self::document)
	// / [`body`](Self::body) are empty; every app renders the shared
	// `legalTakedownTombstone(reference)` (`fauna_core::obligation`) **in place
	// of the bubble** — never a blank / failed-to-decrypt bubble. Best-effort at
	// the relay only: a message a device synced *before* the takedown is deduped
	// in ([`crate::store::threads::ThreadStore::append_message`] keeps the
	// already-present real copy), which is the intended E2E boundary — the nest
	// cannot recall a delivered message. Additive `#[serde(default,
	// skip_serializing_if)]` so the at-rest `ChannelHistorySlice` blob stays
	// byte-identical for a normal message (`version-compatibility.md`;
	// no-user-data-loss).
	LegalTakedownRef *string
	// Per-category content-label verdicts (`moderation.md` § Per-row badge
	// data path, ratified 2026-07-16) — the client-side twin of
	// `fauna_feed::PostSummary::labels`. Populated post-decrypt by
	// [`crate::ConversationsManager::ingest_inbound_to_thread`] from the same
	// `fauna_core::text_heuristic::classify_text` pass that feeds the
	// [`fauna_client_moderation::LocalDetectionStore`] (this content is
	// MLS-sealed at rest — the nest never sees it, so the client is the only
	// place it can be classified). Additive `#[serde(default,
	// skip_serializing_if)]` so the at-rest `ChannelHistorySlice` blob stays
	// byte-identical for an unlabelled message.
	Labels []fauna_core.ContentLabelEntry
	// This message's identity on the **account data plane** — what an app
	// needs to report the T1 body-rendered observation when it paints the body
	// (`account-data-plane.md` § The replica boundary → T1).
	//
	// `Some` exactly for a record the nest sequenced onto a content-scope feed
	// — FaunaMls conversation messages today. Every other rail (SMTP, a bridge,
	// the mock) is not on the plane at all, so `None` there is the honest
	// answer, not a gap.
	//
	// It lives on the snapshot rather than behind a manager lookup because it
	// must survive a restart with the message it names: the snapshot is what
	// [`crate::store::history::ChannelHistorySlice`] persists, so a restored
	// thread can still report observations for messages this device already
	// held. Additive `#[serde(default, skip_serializing_if)]`, so the at-rest
	// slice stays byte-identical for a message that carries none
	// (`version-compatibility.md`).
	PlaneRef *PlaneRef
	// Whether **this viewer** may delete this message — the one fact behind
	// `dm-message-delete-button`, so no app branches on a role: the viewer's
	// own message on a thread that supports delete, or any message when the
	// viewer is owner or admin of a governed end-to-end room
	// (`conversation-rooms.md` § Roles and authorization → *Delete any message
	// — the mechanism* → *The affordance*). Derived on every projection by
	// [`crate::ConversationsManager::thread_detail`], never stored: a role
	// changes without the message changing, so a persisted answer would be a
	// stale one. Additive (`serde` + UniFFI defaults), so the at-rest slice
	// stays byte-identical and no app constructor has to name it.
	CanDelete bool
}

func (r *MessageSnapshot) Destroy() {
	FfiDestroyerTypeMessageId{}.Destroy(r.MessageId)
	FfiDestroyerTypedAddress{}.Destroy(r.Sender)
	FfiDestroyerString{}.Destroy(r.SenderDisplay)
	FfiDestroyerString{}.Destroy(r.Body)
	fauna_core.FfiDestroyerRenderDocument{}.Destroy(r.Document)
	FfiDestroyerInt64{}.Destroy(r.TimestampMs)
	FfiDestroyerOptionalString{}.Destroy(r.SubjectLine)
	FfiDestroyerMessageBadges{}.Destroy(r.Badges)
	FfiDestroyerOptionalTypeMessageId{}.Destroy(r.ReplyTo)
	FfiDestroyerSequenceReactionGroup{}.Destroy(r.Reactions)
	FfiDestroyerBool{}.Destroy(r.Deleted)
	FfiDestroyerBool{}.Destroy(r.IsOwn)
	FfiDestroyerOptionalString{}.Destroy(r.LegalTakedownRef)
	FfiDestroyerSequenceContentLabelEntry{}.Destroy(r.Labels)
	FfiDestroyerOptionalPlaneRef{}.Destroy(r.PlaneRef)
	FfiDestroyerBool{}.Destroy(r.CanDelete)
}

type FfiConverterMessageSnapshot struct{}

var FfiConverterMessageSnapshotINSTANCE = FfiConverterMessageSnapshot{}

func (c FfiConverterMessageSnapshot) Lift(rb RustBufferI) MessageSnapshot {
	return LiftFromRustBuffer[MessageSnapshot](c, rb)
}

func (c FfiConverterMessageSnapshot) Read(reader io.Reader) MessageSnapshot {
	return MessageSnapshot{
		FfiConverterTypeMessageIdINSTANCE.Read(reader),
		FfiConverterTypedAddressINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		fauna_core.FfiConverterRenderDocumentINSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterMessageBadgesINSTANCE.Read(reader),
		FfiConverterOptionalTypeMessageIdINSTANCE.Read(reader),
		FfiConverterSequenceReactionGroupINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterSequenceContentLabelEntryINSTANCE.Read(reader),
		FfiConverterOptionalPlaneRefINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterMessageSnapshot) Lower(value MessageSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[MessageSnapshot](c, value)
}

func (c FfiConverterMessageSnapshot) LowerExternal(value MessageSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[MessageSnapshot](c, value))
}

func (c FfiConverterMessageSnapshot) Write(writer io.Writer, value MessageSnapshot) {
	FfiConverterTypeMessageIdINSTANCE.Write(writer, value.MessageId)
	FfiConverterTypedAddressINSTANCE.Write(writer, value.Sender)
	FfiConverterStringINSTANCE.Write(writer, value.SenderDisplay)
	FfiConverterStringINSTANCE.Write(writer, value.Body)
	fauna_core.FfiConverterRenderDocumentINSTANCE.Write(writer, value.Document)
	FfiConverterInt64INSTANCE.Write(writer, value.TimestampMs)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.SubjectLine)
	FfiConverterMessageBadgesINSTANCE.Write(writer, value.Badges)
	FfiConverterOptionalTypeMessageIdINSTANCE.Write(writer, value.ReplyTo)
	FfiConverterSequenceReactionGroupINSTANCE.Write(writer, value.Reactions)
	FfiConverterBoolINSTANCE.Write(writer, value.Deleted)
	FfiConverterBoolINSTANCE.Write(writer, value.IsOwn)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.LegalTakedownRef)
	FfiConverterSequenceContentLabelEntryINSTANCE.Write(writer, value.Labels)
	FfiConverterOptionalPlaneRefINSTANCE.Write(writer, value.PlaneRef)
	FfiConverterBoolINSTANCE.Write(writer, value.CanDelete)
}

type FfiDestroyerMessageSnapshot struct{}

func (_ FfiDestroyerMessageSnapshot) Destroy(value MessageSnapshot) {
	value.Destroy()
}

// Where one message sits on the account data plane: the content scope whose
// feed carried it, and the record it is on that feed.
//
// Deliberately two *strings* rather than the typed `ContentScope` /
// `ContentHash`: this crate is the conversations model, shared with the Go
// mail bridge and every app's binding, and it has no business depending on the
// sync engine's types. The one consumer that needs typed values —
// `fauna_sync_engine::observation_intake::Observation` — parses them at its own
// seam (`Observation::parse`), so no app hand-parses hex or hand-builds a
// scope string. [`crate::plane`] is where this pair is derived.
type PlaneRef struct {
	// The canonical content-scope string — `content:conv:<channel-hex>` for a
	// conversation message.
	Scope string
	// The record CID's **digest**, lowercase hex. The codec half is not
	// carried because it is not a variable: every record on this plane is
	// dag-cbor-coded, the same assumption the content-scope walk makes when it
	// rebuilds a CID from the feed row's `path_hash`
	// (`fauna_sync_engine::content_scope_plane`).
	RecordDigest string
}

func (r *PlaneRef) Destroy() {
	FfiDestroyerString{}.Destroy(r.Scope)
	FfiDestroyerString{}.Destroy(r.RecordDigest)
}

type FfiConverterPlaneRef struct{}

var FfiConverterPlaneRefINSTANCE = FfiConverterPlaneRef{}

func (c FfiConverterPlaneRef) Lift(rb RustBufferI) PlaneRef {
	return LiftFromRustBuffer[PlaneRef](c, rb)
}

func (c FfiConverterPlaneRef) Read(reader io.Reader) PlaneRef {
	return PlaneRef{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterPlaneRef) Lower(value PlaneRef) C.RustBuffer {
	return LowerIntoRustBuffer[PlaneRef](c, value)
}

func (c FfiConverterPlaneRef) LowerExternal(value PlaneRef) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[PlaneRef](c, value))
}

func (c FfiConverterPlaneRef) Write(writer io.Writer, value PlaneRef) {
	FfiConverterStringINSTANCE.Write(writer, value.Scope)
	FfiConverterStringINSTANCE.Write(writer, value.RecordDigest)
}

type FfiDestroyerPlaneRef struct{}

func (_ FfiDestroyerPlaneRef) Destroy(value PlaneRef) {
	value.Destroy()
}

// Wire-shaped inbound message handed to the manager by a backend.
type RailInboundMessage struct {
	Rail        Rail
	Sender      TypedAddress
	Recipients  []TypedAddress
	Subject     *string
	Body        string
	BodyFormat  BodyFormat
	TimestampMs int64
	MessageId   MessageId
	InReplyTo   *MessageId
	Attachments []AttachmentSnapshot
	Badges      MessageBadges
	// `Some(reference)` iff the nest has taken this message down under a legal
	// obligation (the conversation twin of the post takedown; `moderation.md`
	// § Categories & enforcement item 1). The sealed envelope was **withheld**
	// (empty) from the relay fetch, so there is nothing to decrypt — the driver
	// ([`crate::backends::fauna_mls::poll_inbound_conv`]) builds a tombstone
	// inbound carrying only this reference, and the client renders the shared
	// `legalTakedownTombstone(reference)` in place of the bubble body. `None`
	// for every normal message.
	LegalTakedownRef *string
	// This record's account-data-plane identity, when the rail has one — see
	// [`crate::message::MessageSnapshot::plane_ref`]. Set by the FaunaMls
	// driver, which is the layer that holds all three inputs the derivation
	// needs (channel, seq, the sealed envelope as the nest stored it); `None`
	// on every rail that is not on the plane.
	PlaneRef *PlaneRef
}

func (r *RailInboundMessage) Destroy() {
	FfiDestroyerRail{}.Destroy(r.Rail)
	FfiDestroyerTypedAddress{}.Destroy(r.Sender)
	FfiDestroyerSequenceTypedAddress{}.Destroy(r.Recipients)
	FfiDestroyerOptionalString{}.Destroy(r.Subject)
	FfiDestroyerString{}.Destroy(r.Body)
	FfiDestroyerBodyFormat{}.Destroy(r.BodyFormat)
	FfiDestroyerInt64{}.Destroy(r.TimestampMs)
	FfiDestroyerTypeMessageId{}.Destroy(r.MessageId)
	FfiDestroyerOptionalTypeMessageId{}.Destroy(r.InReplyTo)
	FfiDestroyerSequenceAttachmentSnapshot{}.Destroy(r.Attachments)
	FfiDestroyerMessageBadges{}.Destroy(r.Badges)
	FfiDestroyerOptionalString{}.Destroy(r.LegalTakedownRef)
	FfiDestroyerOptionalPlaneRef{}.Destroy(r.PlaneRef)
}

type FfiConverterRailInboundMessage struct{}

var FfiConverterRailInboundMessageINSTANCE = FfiConverterRailInboundMessage{}

func (c FfiConverterRailInboundMessage) Lift(rb RustBufferI) RailInboundMessage {
	return LiftFromRustBuffer[RailInboundMessage](c, rb)
}

func (c FfiConverterRailInboundMessage) Read(reader io.Reader) RailInboundMessage {
	return RailInboundMessage{
		FfiConverterRailINSTANCE.Read(reader),
		FfiConverterTypedAddressINSTANCE.Read(reader),
		FfiConverterSequenceTypedAddressINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterBodyFormatINSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterTypeMessageIdINSTANCE.Read(reader),
		FfiConverterOptionalTypeMessageIdINSTANCE.Read(reader),
		FfiConverterSequenceAttachmentSnapshotINSTANCE.Read(reader),
		FfiConverterMessageBadgesINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalPlaneRefINSTANCE.Read(reader),
	}
}

func (c FfiConverterRailInboundMessage) Lower(value RailInboundMessage) C.RustBuffer {
	return LowerIntoRustBuffer[RailInboundMessage](c, value)
}

func (c FfiConverterRailInboundMessage) LowerExternal(value RailInboundMessage) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RailInboundMessage](c, value))
}

func (c FfiConverterRailInboundMessage) Write(writer io.Writer, value RailInboundMessage) {
	FfiConverterRailINSTANCE.Write(writer, value.Rail)
	FfiConverterTypedAddressINSTANCE.Write(writer, value.Sender)
	FfiConverterSequenceTypedAddressINSTANCE.Write(writer, value.Recipients)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Subject)
	FfiConverterStringINSTANCE.Write(writer, value.Body)
	FfiConverterBodyFormatINSTANCE.Write(writer, value.BodyFormat)
	FfiConverterInt64INSTANCE.Write(writer, value.TimestampMs)
	FfiConverterTypeMessageIdINSTANCE.Write(writer, value.MessageId)
	FfiConverterOptionalTypeMessageIdINSTANCE.Write(writer, value.InReplyTo)
	FfiConverterSequenceAttachmentSnapshotINSTANCE.Write(writer, value.Attachments)
	FfiConverterMessageBadgesINSTANCE.Write(writer, value.Badges)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.LegalTakedownRef)
	FfiConverterOptionalPlaneRefINSTANCE.Write(writer, value.PlaneRef)
}

type FfiDestroyerRailInboundMessage struct{}

func (_ FfiDestroyerRailInboundMessage) Destroy(value RailInboundMessage) {
	value.Destroy()
}

// The aggregated reaction state for one emoji on a message.
//
// Ordered by first-add appearance in the event sequence. `count` is the
// number of distinct current reactors; `reacted_by_me` is true when the
// local actor is among them.
type ReactionGroup struct {
	// The emoji (Unicode scalar sequence, e.g. `"👍"`, `"❤️"`).
	Emoji string
	// Number of distinct actors currently reacted with this emoji.
	Count uint32
	// Whether the local actor (`me`) is among the current reactors.
	ReactedByMe bool
}

func (r *ReactionGroup) Destroy() {
	FfiDestroyerString{}.Destroy(r.Emoji)
	FfiDestroyerUint32{}.Destroy(r.Count)
	FfiDestroyerBool{}.Destroy(r.ReactedByMe)
}

type FfiConverterReactionGroup struct{}

var FfiConverterReactionGroupINSTANCE = FfiConverterReactionGroup{}

func (c FfiConverterReactionGroup) Lift(rb RustBufferI) ReactionGroup {
	return LiftFromRustBuffer[ReactionGroup](c, rb)
}

func (c FfiConverterReactionGroup) Read(reader io.Reader) ReactionGroup {
	return ReactionGroup{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterReactionGroup) Lower(value ReactionGroup) C.RustBuffer {
	return LowerIntoRustBuffer[ReactionGroup](c, value)
}

func (c FfiConverterReactionGroup) LowerExternal(value ReactionGroup) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ReactionGroup](c, value))
}

func (c FfiConverterReactionGroup) Write(writer io.Writer, value ReactionGroup) {
	FfiConverterStringINSTANCE.Write(writer, value.Emoji)
	FfiConverterUint32INSTANCE.Write(writer, value.Count)
	FfiConverterBoolINSTANCE.Write(writer, value.ReactedByMe)
}

type FfiDestroyerReactionGroup struct{}

func (_ FfiDestroyerReactionGroup) Destroy(value ReactionGroup) {
	value.Destroy()
}

type RecipientPickerState struct {
	RawInput     string
	Chips        []TypedAddress
	Suggestions  []TypedAddress
	ResolveState ResolveState
	// The address the async backend probe (`ConversationsManager::resolve_recipient`)
	// resolved `raw_input` to, when `resolve_state == Resolved`. Carries the
	// *resolved* rail/identity — e.g. a 64-hex actor id promoted to
	// `TypedAddress::Fauna` by the FaunaMls key-package probe — so committing
	// the chip uses it rather than the format-only re-parse (which cannot
	// produce `Fauna`). Cleared whenever `raw_input` changes or a chip commits.
	Resolved *TypedAddress
	// Whether the room about to be created seats the user's **home nest** as a
	// member — `recipient-picker-home-nest-toggle`'s `checked` attribute. The
	// nest is never a chip, so this is how it joins the member set: checked,
	// the room derives `community` (`conversation-rooms.md` § The three
	// classes) and the first send founds one instead of an end-to-end group.
	// A class is chosen only by choosing members, never by a flag on an
	// existing room — this is the member choice, made before the room exists.
	IncludeHomeNest bool
}

func (r *RecipientPickerState) Destroy() {
	FfiDestroyerString{}.Destroy(r.RawInput)
	FfiDestroyerSequenceTypedAddress{}.Destroy(r.Chips)
	FfiDestroyerSequenceTypedAddress{}.Destroy(r.Suggestions)
	FfiDestroyerResolveState{}.Destroy(r.ResolveState)
	FfiDestroyerOptionalTypedAddress{}.Destroy(r.Resolved)
	FfiDestroyerBool{}.Destroy(r.IncludeHomeNest)
}

type FfiConverterRecipientPickerState struct{}

var FfiConverterRecipientPickerStateINSTANCE = FfiConverterRecipientPickerState{}

func (c FfiConverterRecipientPickerState) Lift(rb RustBufferI) RecipientPickerState {
	return LiftFromRustBuffer[RecipientPickerState](c, rb)
}

func (c FfiConverterRecipientPickerState) Read(reader io.Reader) RecipientPickerState {
	return RecipientPickerState{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterSequenceTypedAddressINSTANCE.Read(reader),
		FfiConverterSequenceTypedAddressINSTANCE.Read(reader),
		FfiConverterResolveStateINSTANCE.Read(reader),
		FfiConverterOptionalTypedAddressINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterRecipientPickerState) Lower(value RecipientPickerState) C.RustBuffer {
	return LowerIntoRustBuffer[RecipientPickerState](c, value)
}

func (c FfiConverterRecipientPickerState) LowerExternal(value RecipientPickerState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RecipientPickerState](c, value))
}

func (c FfiConverterRecipientPickerState) Write(writer io.Writer, value RecipientPickerState) {
	FfiConverterStringINSTANCE.Write(writer, value.RawInput)
	FfiConverterSequenceTypedAddressINSTANCE.Write(writer, value.Chips)
	FfiConverterSequenceTypedAddressINSTANCE.Write(writer, value.Suggestions)
	FfiConverterResolveStateINSTANCE.Write(writer, value.ResolveState)
	FfiConverterOptionalTypedAddressINSTANCE.Write(writer, value.Resolved)
	FfiConverterBoolINSTANCE.Write(writer, value.IncludeHomeNest)
}

type FfiDestroyerRecipientPickerState struct{}

func (_ FfiDestroyerRecipientPickerState) Destroy(value RecipientPickerState) {
	value.Destroy()
}

// What the compose bar's reply preview (`dm-reply-preview`) shows for the
// reply in progress: who wrote the answered message and a plain-text excerpt
// of it (`conversations.md` § Layout & flow — "a reply in progress shows the
// message you are answering"). Built once, by
// [`crate::ConversationsManager::reply_preview`], so the apps stop deriving
// three different things (the body, the sender's name, the bare message id).
type ReplyPreview struct {
	// The answered message's sender, as the bubble names it (`dm-sender`).
	SenderDisplay string
	// The answered message as plain text — markdown stripped, bounded like a
	// list row's snippet and cut back to a word boundary.
	Excerpt string
}

func (r *ReplyPreview) Destroy() {
	FfiDestroyerString{}.Destroy(r.SenderDisplay)
	FfiDestroyerString{}.Destroy(r.Excerpt)
}

type FfiConverterReplyPreview struct{}

var FfiConverterReplyPreviewINSTANCE = FfiConverterReplyPreview{}

func (c FfiConverterReplyPreview) Lift(rb RustBufferI) ReplyPreview {
	return LiftFromRustBuffer[ReplyPreview](c, rb)
}

func (c FfiConverterReplyPreview) Read(reader io.Reader) ReplyPreview {
	return ReplyPreview{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterReplyPreview) Lower(value ReplyPreview) C.RustBuffer {
	return LowerIntoRustBuffer[ReplyPreview](c, value)
}

func (c FfiConverterReplyPreview) LowerExternal(value ReplyPreview) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ReplyPreview](c, value))
}

func (c FfiConverterReplyPreview) Write(writer io.Writer, value ReplyPreview) {
	FfiConverterStringINSTANCE.Write(writer, value.SenderDisplay)
	FfiConverterStringINSTANCE.Write(writer, value.Excerpt)
}

type FfiDestroyerReplyPreview struct{}

func (_ FfiDestroyerReplyPreview) Destroy(value ReplyPreview) {
	value.Destroy()
}

// The `recipient-resolve-status` element's render-ready view: the kebab-case
// state `token` (`idle` / `resolving` / `resolved` / `not-found` / `error` —
// the vocabulary `docs/goal/ui/conversations.md` § Errors & edge cases
// ratifies, driving the element's `state` automation attribute) plus the
// status `label` each app resolves through its own i18n pipeline
// (`conversations.unified.recipient_resolve_*`; `None` for `Idle`, which
// renders empty — the [`fauna_core::format`] `Option`-as-signal shape).
//
// Shared so the state→(token, label) map can't drift per-app — previously
// hand-rolled in all five apps (linux/web/android full 5-arm copies;
// windows split across an enum→token hop and a token→text hop; apple carried
// TWO copies, and its folder share-sheet copy covered only 3 arms, so a
// `resolved`/`not-found` recipient rendered a blank status — the drift this
// lift retires, priority #4). Styling of the status line stays per-app.
type ResolveStatusView struct {
	Token string
	Label *fauna_core.LocalizedText
}

func (r *ResolveStatusView) Destroy() {
	FfiDestroyerString{}.Destroy(r.Token)
	FfiDestroyerOptionalLocalizedText{}.Destroy(r.Label)
}

type FfiConverterResolveStatusView struct{}

var FfiConverterResolveStatusViewINSTANCE = FfiConverterResolveStatusView{}

func (c FfiConverterResolveStatusView) Lift(rb RustBufferI) ResolveStatusView {
	return LiftFromRustBuffer[ResolveStatusView](c, rb)
}

func (c FfiConverterResolveStatusView) Read(reader io.Reader) ResolveStatusView {
	return ResolveStatusView{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalLocalizedTextINSTANCE.Read(reader),
	}
}

func (c FfiConverterResolveStatusView) Lower(value ResolveStatusView) C.RustBuffer {
	return LowerIntoRustBuffer[ResolveStatusView](c, value)
}

func (c FfiConverterResolveStatusView) LowerExternal(value ResolveStatusView) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ResolveStatusView](c, value))
}

func (c FfiConverterResolveStatusView) Write(writer io.Writer, value ResolveStatusView) {
	FfiConverterStringINSTANCE.Write(writer, value.Token)
	FfiConverterOptionalLocalizedTextINSTANCE.Write(writer, value.Label)
}

type FfiDestroyerResolveStatusView struct{}

func (_ FfiDestroyerResolveStatusView) Destroy(value ResolveStatusView) {
	value.Destroy()
}

// One room invitation standing for this account, as the conversation list
// paints it (`room-invitation[i]`, with `room-invitation-accept-button[i]` and
// `room-invitation-decline-button[i]`) — already verified against its signer
// ([`RoomInvitation`]), so what it names is what the inviter signed.
type RoomInvitationSnapshot struct {
	// The handle accept and decline name it by — opaque to every app.
	Id int64
	// Who invited, as this device can name them: the handle it has met them
	// under, else their elided actor id (`TypedAddress::display`). Display
	// only — the invitation's authority is the verified signature, never this.
	InviterDisplay string
	// The rank the invitee will hold once they accept.
	Role RoomRole
}

func (r *RoomInvitationSnapshot) Destroy() {
	FfiDestroyerInt64{}.Destroy(r.Id)
	FfiDestroyerString{}.Destroy(r.InviterDisplay)
	FfiDestroyerRoomRole{}.Destroy(r.Role)
}

type FfiConverterRoomInvitationSnapshot struct{}

var FfiConverterRoomInvitationSnapshotINSTANCE = FfiConverterRoomInvitationSnapshot{}

func (c FfiConverterRoomInvitationSnapshot) Lift(rb RustBufferI) RoomInvitationSnapshot {
	return LiftFromRustBuffer[RoomInvitationSnapshot](c, rb)
}

func (c FfiConverterRoomInvitationSnapshot) Read(reader io.Reader) RoomInvitationSnapshot {
	return RoomInvitationSnapshot{
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterRoomRoleINSTANCE.Read(reader),
	}
}

func (c FfiConverterRoomInvitationSnapshot) Lower(value RoomInvitationSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[RoomInvitationSnapshot](c, value)
}

func (c FfiConverterRoomInvitationSnapshot) LowerExternal(value RoomInvitationSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RoomInvitationSnapshot](c, value))
}

func (c FfiConverterRoomInvitationSnapshot) Write(writer io.Writer, value RoomInvitationSnapshot) {
	FfiConverterInt64INSTANCE.Write(writer, value.Id)
	FfiConverterStringINSTANCE.Write(writer, value.InviterDisplay)
	FfiConverterRoomRoleINSTANCE.Write(writer, value.Role)
}

type FfiDestroyerRoomInvitationSnapshot struct{}

func (_ FfiDestroyerRoomInvitationSnapshot) Destroy(value RoomInvitationSnapshot) {
	value.Destroy()
}

// One member's render facts — **index-parallel with
// `ThreadDetail::participants`** (and `participant_displays`), so
// `members[i]` is the role mark on `thread-member-chip[i]`.
type RoomMemberSnapshot struct {
	Kind PrincipalKind
	// `None` on a room with no policy (a policy-less room): there are no roles
	// to mark, not "everyone is a member".
	Role *RoomRole
}

func (r *RoomMemberSnapshot) Destroy() {
	FfiDestroyerPrincipalKind{}.Destroy(r.Kind)
	FfiDestroyerOptionalRoomRole{}.Destroy(r.Role)
}

type FfiConverterRoomMemberSnapshot struct{}

var FfiConverterRoomMemberSnapshotINSTANCE = FfiConverterRoomMemberSnapshot{}

func (c FfiConverterRoomMemberSnapshot) Lift(rb RustBufferI) RoomMemberSnapshot {
	return LiftFromRustBuffer[RoomMemberSnapshot](c, rb)
}

func (c FfiConverterRoomMemberSnapshot) Read(reader io.Reader) RoomMemberSnapshot {
	return RoomMemberSnapshot{
		FfiConverterPrincipalKindINSTANCE.Read(reader),
		FfiConverterOptionalRoomRoleINSTANCE.Read(reader),
	}
}

func (c FfiConverterRoomMemberSnapshot) Lower(value RoomMemberSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[RoomMemberSnapshot](c, value)
}

func (c FfiConverterRoomMemberSnapshot) LowerExternal(value RoomMemberSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RoomMemberSnapshot](c, value))
}

func (c FfiConverterRoomMemberSnapshot) Write(writer io.Writer, value RoomMemberSnapshot) {
	FfiConverterPrincipalKindINSTANCE.Write(writer, value.Kind)
	FfiConverterOptionalRoomRoleINSTANCE.Write(writer, value.Role)
}

type FfiDestroyerRoomMemberSnapshot struct{}

func (_ FfiDestroyerRoomMemberSnapshot) Destroy(value RoomMemberSnapshot) {
	value.Destroy()
}

// One invitation pending on a room, as the room's member surface paints it
// for a viewer who may withdraw it ([`RoomSnapshot::pending_invites`]).
type RoomPendingInviteSnapshot struct {
	// The invitee's actor id, lowercase hex — the handle
	// `ConversationsManager::withdraw_room_invite` names the invitation by.
	// Opaque to every app.
	InviteeActorHex string
	// Who was invited: the `handle@domain` the home nest knows them under,
	// else their elided actor id (`TypedAddress::display`). Display only.
	InviteeDisplay string
	// Who invited them, named the same way. Attribution — never re-pointed by
	// a succession.
	InviterDisplay string
	// The rank on offer — admin or member, never owner.
	Role RoomRole
	// When the invitation was (last) issued, epoch millis.
	InvitedAtMs int64
	// Whether the invitation has **lapsed in waiting**: the accept door would
	// refuse it today (its inviter was demoted, departed or succeeded, or the
	// policy no longer names an admin invitee). It seats nobody and is listed
	// so it can be cleared rather than sit invisible until somebody tries it.
	Lapsed bool
}

func (r *RoomPendingInviteSnapshot) Destroy() {
	FfiDestroyerString{}.Destroy(r.InviteeActorHex)
	FfiDestroyerString{}.Destroy(r.InviteeDisplay)
	FfiDestroyerString{}.Destroy(r.InviterDisplay)
	FfiDestroyerRoomRole{}.Destroy(r.Role)
	FfiDestroyerInt64{}.Destroy(r.InvitedAtMs)
	FfiDestroyerBool{}.Destroy(r.Lapsed)
}

type FfiConverterRoomPendingInviteSnapshot struct{}

var FfiConverterRoomPendingInviteSnapshotINSTANCE = FfiConverterRoomPendingInviteSnapshot{}

func (c FfiConverterRoomPendingInviteSnapshot) Lift(rb RustBufferI) RoomPendingInviteSnapshot {
	return LiftFromRustBuffer[RoomPendingInviteSnapshot](c, rb)
}

func (c FfiConverterRoomPendingInviteSnapshot) Read(reader io.Reader) RoomPendingInviteSnapshot {
	return RoomPendingInviteSnapshot{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterRoomRoleINSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterRoomPendingInviteSnapshot) Lower(value RoomPendingInviteSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[RoomPendingInviteSnapshot](c, value)
}

func (c FfiConverterRoomPendingInviteSnapshot) LowerExternal(value RoomPendingInviteSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RoomPendingInviteSnapshot](c, value))
}

func (c FfiConverterRoomPendingInviteSnapshot) Write(writer io.Writer, value RoomPendingInviteSnapshot) {
	FfiConverterStringINSTANCE.Write(writer, value.InviteeActorHex)
	FfiConverterStringINSTANCE.Write(writer, value.InviteeDisplay)
	FfiConverterStringINSTANCE.Write(writer, value.InviterDisplay)
	FfiConverterRoomRoleINSTANCE.Write(writer, value.Role)
	FfiConverterInt64INSTANCE.Write(writer, value.InvitedAtMs)
	FfiConverterBoolINSTANCE.Write(writer, value.Lapsed)
}

type FfiDestroyerRoomPendingInviteSnapshot struct{}

func (_ FfiDestroyerRoomPendingInviteSnapshot) Destroy(value RoomPendingInviteSnapshot) {
	value.Destroy()
}

// The owner-signed policy's rendered fields (`conversation-rooms.md`
// § Roles and authorization — the policy editor's values).
type RoomPolicySnapshot struct {
	Version       uint64
	Name          *string
	JoinRule      JoinRule
	HistoryPolicy HistoryPolicy
}

func (r *RoomPolicySnapshot) Destroy() {
	FfiDestroyerUint64{}.Destroy(r.Version)
	FfiDestroyerOptionalString{}.Destroy(r.Name)
	FfiDestroyerJoinRule{}.Destroy(r.JoinRule)
	FfiDestroyerHistoryPolicy{}.Destroy(r.HistoryPolicy)
}

type FfiConverterRoomPolicySnapshot struct{}

var FfiConverterRoomPolicySnapshotINSTANCE = FfiConverterRoomPolicySnapshot{}

func (c FfiConverterRoomPolicySnapshot) Lift(rb RustBufferI) RoomPolicySnapshot {
	return LiftFromRustBuffer[RoomPolicySnapshot](c, rb)
}

func (c FfiConverterRoomPolicySnapshot) Read(reader io.Reader) RoomPolicySnapshot {
	return RoomPolicySnapshot{
		FfiConverterUint64INSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterJoinRuleINSTANCE.Read(reader),
		FfiConverterHistoryPolicyINSTANCE.Read(reader),
	}
}

func (c FfiConverterRoomPolicySnapshot) Lower(value RoomPolicySnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[RoomPolicySnapshot](c, value)
}

func (c FfiConverterRoomPolicySnapshot) LowerExternal(value RoomPolicySnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RoomPolicySnapshot](c, value))
}

func (c FfiConverterRoomPolicySnapshot) Write(writer io.Writer, value RoomPolicySnapshot) {
	FfiConverterUint64INSTANCE.Write(writer, value.Version)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Name)
	FfiConverterJoinRuleINSTANCE.Write(writer, value.JoinRule)
	FfiConverterHistoryPolicyINSTANCE.Write(writer, value.HistoryPolicy)
}

type FfiDestroyerRoomPolicySnapshot struct{}

func (_ FfiDestroyerRoomPolicySnapshot) Destroy(value RoomPolicySnapshot) {
	value.Destroy()
}

// The `room_settings` editor's staged state, seeded from the projected room
// and diffed back to [`RoomSettingsEdit`]s on Save.
//
// Every `Vec` here is index-parallel **with each other and with
// [`Self::seats`]** — the roster as it stood when the editor opened. They are
// deliberately NOT parallel with the *live* `ThreadDetail::participants`,
// which moves under an open editor: an inbound membership commit drops a row
// from the middle of that list (`ConversationsManager::apply_inbound_roster`
// → `ThreadStore::retain_participants`), shifting every later participant one
// place left.
//
// ⚠ **So every method here takes the live participant list and resolves
// through [`Self::seats`] — never by position.** Staging by position let a
// roster that shifted mid-edit carry the owner's choice onto whoever slid
// into that slot, and `update_room_policy` signs the result with the owner's
// own credential, so every member verifies and accepts an appointment the
// owner never made. The paint index
// (`room-admin-toggle[i]`) is still a live-list index — that is what the
// element contract means — which is exactly why translating it here, once,
// is the fix rather than pushing the resolution into seven painters.
type RoomSettingsDraft struct {
	// `room-join-rule-select`'s staged value.
	JoinRule JoinRule
	// `room-history-policy-select`'s staged value.
	HistoryPolicy HistoryPolicy
	// **The identity column: the actor each staged row belongs to**, as
	// lowercase hex, index-parallel with every other `Vec` here. Empty for a
	// participant that carries no actor id — a non-`Fauna` row, which is
	// never a staging target anyway ([`Self::eligible`]).
	//
	// This is what makes the draft survive a roster that moves under it: a
	// live participant is matched to its staged flags by actor, and the two
	// residues are both handled honestly — a staged row that has since LEFT
	// emits nothing (`update_room_policy` has no membership check of its own,
	// so emitting it would write a departed member into the room's signed
	// policy), and a member seated after the editor opened is simply not in
	// this column, so Save says nothing about them either way.
	Seats []string
	// The staged admin flag per seeded row — `room-admin-toggle[i]`'s
	// `checked` attribute, once `i` has been resolved through
	// [`Self::seats`].
	Admins []bool
	// Whether that participant is the owner — never a toggle target: the
	// owner is not in the admin set by construction (`RoomPolicy::validate`),
	// and neither control is ever live on the owner's own row.
	Owners []bool
	// Whether either control may act on that row **at all**, before the
	// viewer's capability is consulted: a `Fauna` participant (a room's
	// members are user principals) who is not the owner. An app paints
	// `room-admin-toggle[i]` live iff
	// `capabilities.can_appoint_admins && eligible[i]`, and
	// `room-owner-transfer-button[i]` live iff
	// `capabilities.can_transfer_ownership && eligible[i]` — the one
	// expression every app writes, so no app re-derives who is eligible
	// (`ui/conversations.md` § Architectural rules 5: greyed, never hidden).
	Eligible []bool
	// The participant staged as the room's new owner
	// (`room-owner-transfer-button[i]`), at most one; `None` stages no
	// hand-over.
	//
	// An index into the **seeded** vectors, not into the live participant
	// list — the seeded vectors never change length or order after
	// [`Self::seed`], so this stays valid however the roster moves, and
	// [`Self::seats`] says who it means.
	TransferTo *uint32
	// The projected join rule the staged one is diffed against. Set by
	// [`RoomSettingsDraft::seed`] and never edited by a painter.
	SeedJoinRule JoinRule
	// The projected history policy the staged one is diffed against.
	SeedHistoryPolicy HistoryPolicy
	// The projected admin flags the staged ones are diffed against.
	SeedAdmins []bool
	// `room-nest-read-toggle`'s staged value — whether the home nest reads
	// the room. `None` where there is no such read to stage: an end-to-end
	// room, or a community room whose answer this device has not read yet
	// (`RoomSnapshot::nest_read`); the toggle is not painted there.
	NestRead *bool
	// The projected nest read the staged one is diffed against.
	SeedNestRead *bool
	// The staged labeler set — published labeler ids, lowercase hex, sorted —
	// the editor's per-labeler toggle state. `None` where there is none to
	// stage (`RoomSnapshot::labelers`): the control is not painted there.
	// At most `fauna_mls::room_policy::MAX_ROOM_LABELERS` long.
	Labelers *[]string
	// The projected set the staged one is diffed against.
	SeedLabelers *[]string
}

func (r *RoomSettingsDraft) Destroy() {
	FfiDestroyerJoinRule{}.Destroy(r.JoinRule)
	FfiDestroyerHistoryPolicy{}.Destroy(r.HistoryPolicy)
	FfiDestroyerSequenceString{}.Destroy(r.Seats)
	FfiDestroyerSequenceBool{}.Destroy(r.Admins)
	FfiDestroyerSequenceBool{}.Destroy(r.Owners)
	FfiDestroyerSequenceBool{}.Destroy(r.Eligible)
	FfiDestroyerOptionalUint32{}.Destroy(r.TransferTo)
	FfiDestroyerJoinRule{}.Destroy(r.SeedJoinRule)
	FfiDestroyerHistoryPolicy{}.Destroy(r.SeedHistoryPolicy)
	FfiDestroyerSequenceBool{}.Destroy(r.SeedAdmins)
	FfiDestroyerOptionalBool{}.Destroy(r.NestRead)
	FfiDestroyerOptionalBool{}.Destroy(r.SeedNestRead)
	FfiDestroyerOptionalSequenceString{}.Destroy(r.Labelers)
	FfiDestroyerOptionalSequenceString{}.Destroy(r.SeedLabelers)
}

type FfiConverterRoomSettingsDraft struct{}

var FfiConverterRoomSettingsDraftINSTANCE = FfiConverterRoomSettingsDraft{}

func (c FfiConverterRoomSettingsDraft) Lift(rb RustBufferI) RoomSettingsDraft {
	return LiftFromRustBuffer[RoomSettingsDraft](c, rb)
}

func (c FfiConverterRoomSettingsDraft) Read(reader io.Reader) RoomSettingsDraft {
	return RoomSettingsDraft{
		FfiConverterJoinRuleINSTANCE.Read(reader),
		FfiConverterHistoryPolicyINSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterSequenceBoolINSTANCE.Read(reader),
		FfiConverterSequenceBoolINSTANCE.Read(reader),
		FfiConverterSequenceBoolINSTANCE.Read(reader),
		FfiConverterOptionalUint32INSTANCE.Read(reader),
		FfiConverterJoinRuleINSTANCE.Read(reader),
		FfiConverterHistoryPolicyINSTANCE.Read(reader),
		FfiConverterSequenceBoolINSTANCE.Read(reader),
		FfiConverterOptionalBoolINSTANCE.Read(reader),
		FfiConverterOptionalBoolINSTANCE.Read(reader),
		FfiConverterOptionalSequenceStringINSTANCE.Read(reader),
		FfiConverterOptionalSequenceStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterRoomSettingsDraft) Lower(value RoomSettingsDraft) C.RustBuffer {
	return LowerIntoRustBuffer[RoomSettingsDraft](c, value)
}

func (c FfiConverterRoomSettingsDraft) LowerExternal(value RoomSettingsDraft) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RoomSettingsDraft](c, value))
}

func (c FfiConverterRoomSettingsDraft) Write(writer io.Writer, value RoomSettingsDraft) {
	FfiConverterJoinRuleINSTANCE.Write(writer, value.JoinRule)
	FfiConverterHistoryPolicyINSTANCE.Write(writer, value.HistoryPolicy)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.Seats)
	FfiConverterSequenceBoolINSTANCE.Write(writer, value.Admins)
	FfiConverterSequenceBoolINSTANCE.Write(writer, value.Owners)
	FfiConverterSequenceBoolINSTANCE.Write(writer, value.Eligible)
	FfiConverterOptionalUint32INSTANCE.Write(writer, value.TransferTo)
	FfiConverterJoinRuleINSTANCE.Write(writer, value.SeedJoinRule)
	FfiConverterHistoryPolicyINSTANCE.Write(writer, value.SeedHistoryPolicy)
	FfiConverterSequenceBoolINSTANCE.Write(writer, value.SeedAdmins)
	FfiConverterOptionalBoolINSTANCE.Write(writer, value.NestRead)
	FfiConverterOptionalBoolINSTANCE.Write(writer, value.SeedNestRead)
	FfiConverterOptionalSequenceStringINSTANCE.Write(writer, value.Labelers)
	FfiConverterOptionalSequenceStringINSTANCE.Write(writer, value.SeedLabelers)
}

type FfiDestroyerRoomSettingsDraft struct{}

func (_ FfiDestroyerRoomSettingsDraft) Destroy(value RoomSettingsDraft) {
	value.Destroy()
}

// The room a thread is, as every app paints it.
type RoomSnapshot struct {
	// Derived from `members` by [`derive_room_class`]; rendered on the
	// thread header, and as `ThreadCapabilities::encryption`.
	Class RoomClass
	// Index-parallel with `ThreadDetail::participants`.
	Members []RoomMemberSnapshot
	// `None` on a policy-less room — a 1:1, or a group whose context
	// carries no policy (every group this app forks is born governed; a
	// policy-less group is one a peer minted without a policy).
	Policy *RoomPolicySnapshot
	// The viewer's own effective role; `None` on a policy-less room.
	MyRole *RoomRole
	// Whether the room's home nest reads it — its current generation carries
	// a wrap to the nest. `room-nest-read-toggle`'s `checked` attribute.
	//
	// `Some` only for a **community** room whose floor this device has read
	// from a nest that answers it; `None` for every other class (there is no
	// home-nest read to speak of) and for a community room whose answer is not
	// in yet, where the toggle is not painted rather than painted as a guess.
	// A community room whose nest does not read is still a community room —
	// the nest stays on the floor; the members have withdrawn its grant
	// (`conversation-rooms.md` § Implementation status today, the revoke).
	NestRead *bool
	// The transparent labelers the room's home nest applies to its messages —
	// the room's signed labeler set, **verified** (its signature, and that it
	// names this room), as published labeler ids in lowercase hex
	// (`conversation-rooms.md` § The three classes → *What the home nest does
	// with its read*, purpose 2). What the editor stages from, and what an app
	// says reads the room.
	//
	// `Some(empty)` for a community room that names none. `None` for every
	// other class (no nest reads, so none labels), for a community room whose
	// floor this device has not read, and for one whose stored set does not
	// verify — nothing this device can stand behind, so nothing painted.
	//
	// A set stays in force whoever signed it, including a signer who has since
	// lost the rank to sign another: the home nest runs the stored set, exactly
	// as a policy version outlives its signer's demotion — so it renders as in
	// force, and the next owner or admin to change it replaces it whole.
	Labelers *[]string
	// Whether this is a **community room this account is not keyed into
	// yet** — it accepted, and no owner's or admin's device has wrapped the
	// room's key to it — so nothing sealed in the room opens here until one
	// does. What the room's own row says while it waits, rather than a page
	// error: the wait is an expected state, not a fault
	// (`community-rooms.md` § Implementation status today → *A newcomer's walk
	// waits for its key-in*).
	//
	// `false` for every other class and for a room whose walk has not yet met
	// a record it cannot open. Painted as `thread-room-notice`'s
	// `awaiting-key` state ([`RoomSnapshot::notice`]).
	AwaitingKey bool
	// Whether this room carries **moderation this device could not verify**:
	// an owner's or admin's floor delete record is parked because the chain
	// under the policy version it names rests on a name this device holds no
	// anchor for, and the peer-anchor harvest has **settled** that name for
	// the session without finding one — a retired owner or admin homed on
	// another nest, which the harvest's reach never covers
	// (`conversation-rooms.md` § Roles and authorization → *Delete any
	// message — the mechanism* → *Members verify what they paint*, the
	// "says so" rule). The record's target stays painted, exactly as before:
	// what this adds is that the room says so, instead of showing the message
	// as if nobody had acted on it. A room-level statement, never a mark on
	// the target — that would paint the record's claim before it is verified.
	//
	// `false` for every other class, for a room with nothing parked, and while
	// a parked record still waits on a harvest that could yet anchor its name
	// — then it is *not yet* verified, not *unverifiable*, and the next pass
	// may paint it. Clears when the parked records paint. Across a relaunch
	// the parked set rides the `history/<ch>` replica and is taken up again
	// on the first pass over the room, so the notice returns with it once the
	// harvest has settled the name again (the harvest's own memory is per
	// session). Painted as `thread-room-notice`'s `moderation-unverified`
	// state ([`RoomSnapshot::notice`]), which yields to `awaiting_key`.
	ModerationUnverified bool
	// The invitations pending on this room that the viewer may withdraw,
	// oldest first (`conversation-rooms.md` § Join rules and invites →
	// *Pending invitations are visible to whoever may withdraw them*). The
	// home nest scopes the list — everything for the owner and admins, what
	// it issued for any other seated member — so **every row served carries
	// the withdraw gesture** and no app gates it a second time
	// (`ConversationsManager::withdraw_room_invite`).
	//
	// `Some(empty)` is a real answer: nothing the viewer may withdraw stands
	// pending. `None` for every other class (an end-to-end room's floor
	// holds no invitations), for a community room homed on another nest (the
	// doors have no relay), for one whose list this device has not read, and
	// when the nest's reply omits the list — not painted rather than painted as a
	// guess. Not painted by any app yet: it needs element ids ui.yaml does
	// not carry (rule A).
	PendingInvites *[]RoomPendingInviteSnapshot
}

func (r *RoomSnapshot) Destroy() {
	FfiDestroyerRoomClass{}.Destroy(r.Class)
	FfiDestroyerSequenceRoomMemberSnapshot{}.Destroy(r.Members)
	FfiDestroyerOptionalRoomPolicySnapshot{}.Destroy(r.Policy)
	FfiDestroyerOptionalRoomRole{}.Destroy(r.MyRole)
	FfiDestroyerOptionalBool{}.Destroy(r.NestRead)
	FfiDestroyerOptionalSequenceString{}.Destroy(r.Labelers)
	FfiDestroyerBool{}.Destroy(r.AwaitingKey)
	FfiDestroyerBool{}.Destroy(r.ModerationUnverified)
	FfiDestroyerOptionalSequenceRoomPendingInviteSnapshot{}.Destroy(r.PendingInvites)
}

type FfiConverterRoomSnapshot struct{}

var FfiConverterRoomSnapshotINSTANCE = FfiConverterRoomSnapshot{}

func (c FfiConverterRoomSnapshot) Lift(rb RustBufferI) RoomSnapshot {
	return LiftFromRustBuffer[RoomSnapshot](c, rb)
}

func (c FfiConverterRoomSnapshot) Read(reader io.Reader) RoomSnapshot {
	return RoomSnapshot{
		FfiConverterRoomClassINSTANCE.Read(reader),
		FfiConverterSequenceRoomMemberSnapshotINSTANCE.Read(reader),
		FfiConverterOptionalRoomPolicySnapshotINSTANCE.Read(reader),
		FfiConverterOptionalRoomRoleINSTANCE.Read(reader),
		FfiConverterOptionalBoolINSTANCE.Read(reader),
		FfiConverterOptionalSequenceStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterOptionalSequenceRoomPendingInviteSnapshotINSTANCE.Read(reader),
	}
}

func (c FfiConverterRoomSnapshot) Lower(value RoomSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[RoomSnapshot](c, value)
}

func (c FfiConverterRoomSnapshot) LowerExternal(value RoomSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RoomSnapshot](c, value))
}

func (c FfiConverterRoomSnapshot) Write(writer io.Writer, value RoomSnapshot) {
	FfiConverterRoomClassINSTANCE.Write(writer, value.Class)
	FfiConverterSequenceRoomMemberSnapshotINSTANCE.Write(writer, value.Members)
	FfiConverterOptionalRoomPolicySnapshotINSTANCE.Write(writer, value.Policy)
	FfiConverterOptionalRoomRoleINSTANCE.Write(writer, value.MyRole)
	FfiConverterOptionalBoolINSTANCE.Write(writer, value.NestRead)
	FfiConverterOptionalSequenceStringINSTANCE.Write(writer, value.Labelers)
	FfiConverterBoolINSTANCE.Write(writer, value.AwaitingKey)
	FfiConverterBoolINSTANCE.Write(writer, value.ModerationUnverified)
	FfiConverterOptionalSequenceRoomPendingInviteSnapshotINSTANCE.Write(writer, value.PendingInvites)
}

type FfiDestroyerRoomSnapshot struct{}

func (_ FfiDestroyerRoomSnapshot) Destroy(value RoomSnapshot) {
	value.Destroy()
}

type ThreadCapabilities struct {
	SupportsAttachments      bool
	SupportsMarkdown         bool
	SupportsReactions        bool
	SupportsMessageDelete    bool
	SupportsPerMessageReply  bool
	SupportsMembershipChange bool
	// Whether replies carry an editable **recipient set** — the To/Cc of the
	// message about to be sent, rendered as the always-visible "To" line in
	// the compose bar (`conversations.md` § Participants vs reply recipients).
	// Mail only: each reply picks its recipients (reply vs reply-all, then
	// editable). On FaunaMls/social rails the recipients ARE the thread
	// membership, so there's nothing per-reply to choose — `false`, and the
	// To line is hidden. Distinct from `supports_membership_change` (whether
	// the *thread's* historical participant set is mutable).
	SupportsRecipientSelection bool
	// Rename is offered (an MLS group). On a **governed** room — one carrying
	// a room policy — it is additionally the viewer's role that decides:
	// owner and admin only ([`crate::room::RoomSnapshot::gate`]).
	SupportsRename  bool
	SupportsSubject bool
	DeliveryMode    DeliveryMode
	Encryption      ThreadEncryption
	// The **role-gated** half (`conversation-rooms.md` § Roles and
	// authorization), overlaid by [`crate::room::RoomSnapshot::gate`] from
	// the viewer's effective role on a governed room and left **open**
	// wherever no policy governs. Apps grey `thread-add-participant-button`
	// on `!can_invite` and the member chip's remove on
	// `!can_remove_members`, and never branch on a role themselves
	// (`conversations.md` § Architectural rules 5). Default-valued across
	// UniFFI so an app-side constructor written before these fields existed
	// still builds.
	CanInvite        bool
	CanRemoveMembers bool
	// The policy editor (join rule, history policy): owner/admin of a
	// governed room; `false` wherever there is no policy to edit.
	CanSetPolicy bool
	// Appoint / demote admins: the owner of a governed room only.
	CanAppointAdmins bool
	// Hand the room to another member (`conversation-rooms.md` § Roles and
	// authorization → *Ownership transfer*): the owner of a governed room
	// only. Its own field rather than a reading of `can_appoint_admins`
	// because the table lists the two rows separately and a nest-enforced
	// class may answer them differently.
	CanTransferOwnership bool
	// Walk out of this room — the roles table's *leave (remove self)* row
	// (`conversation-rooms.md` § Roles and authorization → *Leaving — the
	// mechanism*): admin and member of a governed room, and any member of a
	// policy-less one; **never the owner**, who transfers ownership first because
	// a room is never owner-less. Its own field rather than a reading of
	// `can_remove_members`, which answers the opposite question — an
	// ordinary member may leave and may not remove.
	//
	// `false` on a 1:1: leaving the only other person in a DM is the
	// ordinary thread delete, not a membership act
	// ([`crate::manager::ConversationsManager::remove_participant`] draws the
	// same line).
	CanLeaveRoom bool
}

func (r *ThreadCapabilities) Destroy() {
	FfiDestroyerBool{}.Destroy(r.SupportsAttachments)
	FfiDestroyerBool{}.Destroy(r.SupportsMarkdown)
	FfiDestroyerBool{}.Destroy(r.SupportsReactions)
	FfiDestroyerBool{}.Destroy(r.SupportsMessageDelete)
	FfiDestroyerBool{}.Destroy(r.SupportsPerMessageReply)
	FfiDestroyerBool{}.Destroy(r.SupportsMembershipChange)
	FfiDestroyerBool{}.Destroy(r.SupportsRecipientSelection)
	FfiDestroyerBool{}.Destroy(r.SupportsRename)
	FfiDestroyerBool{}.Destroy(r.SupportsSubject)
	FfiDestroyerDeliveryMode{}.Destroy(r.DeliveryMode)
	FfiDestroyerThreadEncryption{}.Destroy(r.Encryption)
	FfiDestroyerBool{}.Destroy(r.CanInvite)
	FfiDestroyerBool{}.Destroy(r.CanRemoveMembers)
	FfiDestroyerBool{}.Destroy(r.CanSetPolicy)
	FfiDestroyerBool{}.Destroy(r.CanAppointAdmins)
	FfiDestroyerBool{}.Destroy(r.CanTransferOwnership)
	FfiDestroyerBool{}.Destroy(r.CanLeaveRoom)
}

type FfiConverterThreadCapabilities struct{}

var FfiConverterThreadCapabilitiesINSTANCE = FfiConverterThreadCapabilities{}

func (c FfiConverterThreadCapabilities) Lift(rb RustBufferI) ThreadCapabilities {
	return LiftFromRustBuffer[ThreadCapabilities](c, rb)
}

func (c FfiConverterThreadCapabilities) Read(reader io.Reader) ThreadCapabilities {
	return ThreadCapabilities{
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterDeliveryModeINSTANCE.Read(reader),
		FfiConverterThreadEncryptionINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterThreadCapabilities) Lower(value ThreadCapabilities) C.RustBuffer {
	return LowerIntoRustBuffer[ThreadCapabilities](c, value)
}

func (c FfiConverterThreadCapabilities) LowerExternal(value ThreadCapabilities) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ThreadCapabilities](c, value))
}

func (c FfiConverterThreadCapabilities) Write(writer io.Writer, value ThreadCapabilities) {
	FfiConverterBoolINSTANCE.Write(writer, value.SupportsAttachments)
	FfiConverterBoolINSTANCE.Write(writer, value.SupportsMarkdown)
	FfiConverterBoolINSTANCE.Write(writer, value.SupportsReactions)
	FfiConverterBoolINSTANCE.Write(writer, value.SupportsMessageDelete)
	FfiConverterBoolINSTANCE.Write(writer, value.SupportsPerMessageReply)
	FfiConverterBoolINSTANCE.Write(writer, value.SupportsMembershipChange)
	FfiConverterBoolINSTANCE.Write(writer, value.SupportsRecipientSelection)
	FfiConverterBoolINSTANCE.Write(writer, value.SupportsRename)
	FfiConverterBoolINSTANCE.Write(writer, value.SupportsSubject)
	FfiConverterDeliveryModeINSTANCE.Write(writer, value.DeliveryMode)
	FfiConverterThreadEncryptionINSTANCE.Write(writer, value.Encryption)
	FfiConverterBoolINSTANCE.Write(writer, value.CanInvite)
	FfiConverterBoolINSTANCE.Write(writer, value.CanRemoveMembers)
	FfiConverterBoolINSTANCE.Write(writer, value.CanSetPolicy)
	FfiConverterBoolINSTANCE.Write(writer, value.CanAppointAdmins)
	FfiConverterBoolINSTANCE.Write(writer, value.CanTransferOwnership)
	FfiConverterBoolINSTANCE.Write(writer, value.CanLeaveRoom)
}

type FfiDestroyerThreadCapabilities struct{}

func (_ FfiDestroyerThreadCapabilities) Destroy(value ThreadCapabilities) {
	value.Destroy()
}

type ThreadDetail struct {
	ThreadId ThreadId
	Rail     Rail
	// The canonical icon concept for `rail` (`rail.glyph()`); see
	// [`ThreadSummary::glyph`]. Lets the thread header reuse the same single
	// per-app `SourceGlyph → native asset` map as the conversation list.
	Glyph               fauna_core.SourceGlyph
	Flavor              ThreadFlavor
	Label               string
	Participants        []TypedAddress
	ParticipantDisplays []string
	Capabilities        ThreadCapabilities
	Messages            []MessageSnapshot
	Compose             ComposeState
	// The one message in [`messages`](Self::messages) the view should
	// distinguish, or `None` — the second half of `SearchNav::Mail`'s contract,
	// *"open the thread **and** select this message in it"*
	// (`docs/goal/ui/search.md` § State & data shape).
	//
	// A **read-time resolve**, not stored state: `ConversationsManager::
	// thread_detail` re-derives it on every emit and yields `None` unless the
	// id is present in `messages`, so an app can never be handed a selection it
	// cannot place. Apps paint from this field alone and never re-derive which
	// message is selected (priority #2); the marker's *shape* is per-platform
	// (a background tint where there is fill to vary, a text marker in a
	// terminal), its automation observable is the shared `selected` attribute
	// on `dm-message-timestamp` (ui.yaml `dm-message-bubble`).
	//
	// Lives on the detail rather than on each [`MessageSnapshot`] because it is
	// *view* state, not a fact about the message — the same split that puts
	// `selected_thread_id` on [`ConversationsSnapshot`] and not on
	// [`ThreadSummary`]. It also keeps the concept out of `MessageSnapshot`,
	// which the Go mail bridge consumes over FFI and has no view to select in.
	SelectedMessageId *MessageId
	// The room this thread is — class, per-member roles (index-parallel
	// with `participants`), the policy, the viewer's role
	// (`conversation-rooms.md` § The room; [`crate::room`]). A **read-time
	// projection** like `selected_message_id`: `ConversationsManager::
	// thread_detail` asks the thread's rail on every emit, and the same
	// projection overlays the role gating onto `capabilities`. `None` for a
	// rail that models no room yet (every non-native rail until the
	// `Bridged` adapter's derivation lands, `conversations.md`
	// § Implementation status today).
	Room *RoomSnapshot
	// Which bridge carries this thread — see [`ThreadSummary::bridge`]; the
	// same read-time projection, made by `ConversationsManager::thread_detail`
	// beside [`Self::room`].
	Bridge *fauna_core.BridgeIdentitySnapshot
	// The family gate's marker — see [`ThreadSummary::guardian_state`].
	GuardianState *GuardianState
}

func (r *ThreadDetail) Destroy() {
	FfiDestroyerTypeThreadId{}.Destroy(r.ThreadId)
	FfiDestroyerRail{}.Destroy(r.Rail)
	fauna_core.FfiDestroyerSourceGlyph{}.Destroy(r.Glyph)
	FfiDestroyerThreadFlavor{}.Destroy(r.Flavor)
	FfiDestroyerString{}.Destroy(r.Label)
	FfiDestroyerSequenceTypedAddress{}.Destroy(r.Participants)
	FfiDestroyerSequenceString{}.Destroy(r.ParticipantDisplays)
	FfiDestroyerThreadCapabilities{}.Destroy(r.Capabilities)
	FfiDestroyerSequenceMessageSnapshot{}.Destroy(r.Messages)
	FfiDestroyerComposeState{}.Destroy(r.Compose)
	FfiDestroyerOptionalTypeMessageId{}.Destroy(r.SelectedMessageId)
	FfiDestroyerOptionalRoomSnapshot{}.Destroy(r.Room)
	FfiDestroyerOptionalBridgeIdentitySnapshot{}.Destroy(r.Bridge)
	FfiDestroyerOptionalGuardianState{}.Destroy(r.GuardianState)
}

type FfiConverterThreadDetail struct{}

var FfiConverterThreadDetailINSTANCE = FfiConverterThreadDetail{}

func (c FfiConverterThreadDetail) Lift(rb RustBufferI) ThreadDetail {
	return LiftFromRustBuffer[ThreadDetail](c, rb)
}

func (c FfiConverterThreadDetail) Read(reader io.Reader) ThreadDetail {
	return ThreadDetail{
		FfiConverterTypeThreadIdINSTANCE.Read(reader),
		FfiConverterRailINSTANCE.Read(reader),
		fauna_core.FfiConverterSourceGlyphINSTANCE.Read(reader),
		FfiConverterThreadFlavorINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterSequenceTypedAddressINSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterThreadCapabilitiesINSTANCE.Read(reader),
		FfiConverterSequenceMessageSnapshotINSTANCE.Read(reader),
		FfiConverterComposeStateINSTANCE.Read(reader),
		FfiConverterOptionalTypeMessageIdINSTANCE.Read(reader),
		FfiConverterOptionalRoomSnapshotINSTANCE.Read(reader),
		FfiConverterOptionalBridgeIdentitySnapshotINSTANCE.Read(reader),
		FfiConverterOptionalGuardianStateINSTANCE.Read(reader),
	}
}

func (c FfiConverterThreadDetail) Lower(value ThreadDetail) C.RustBuffer {
	return LowerIntoRustBuffer[ThreadDetail](c, value)
}

func (c FfiConverterThreadDetail) LowerExternal(value ThreadDetail) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ThreadDetail](c, value))
}

func (c FfiConverterThreadDetail) Write(writer io.Writer, value ThreadDetail) {
	FfiConverterTypeThreadIdINSTANCE.Write(writer, value.ThreadId)
	FfiConverterRailINSTANCE.Write(writer, value.Rail)
	fauna_core.FfiConverterSourceGlyphINSTANCE.Write(writer, value.Glyph)
	FfiConverterThreadFlavorINSTANCE.Write(writer, value.Flavor)
	FfiConverterStringINSTANCE.Write(writer, value.Label)
	FfiConverterSequenceTypedAddressINSTANCE.Write(writer, value.Participants)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.ParticipantDisplays)
	FfiConverterThreadCapabilitiesINSTANCE.Write(writer, value.Capabilities)
	FfiConverterSequenceMessageSnapshotINSTANCE.Write(writer, value.Messages)
	FfiConverterComposeStateINSTANCE.Write(writer, value.Compose)
	FfiConverterOptionalTypeMessageIdINSTANCE.Write(writer, value.SelectedMessageId)
	FfiConverterOptionalRoomSnapshotINSTANCE.Write(writer, value.Room)
	FfiConverterOptionalBridgeIdentitySnapshotINSTANCE.Write(writer, value.Bridge)
	FfiConverterOptionalGuardianStateINSTANCE.Write(writer, value.GuardianState)
}

type FfiDestroyerThreadDetail struct{}

func (_ FfiDestroyerThreadDetail) Destroy(value ThreadDetail) {
	value.Destroy()
}

type ThreadSummary struct {
	ThreadId ThreadId
	Rail     Rail
	// The canonical icon concept for `rail` (`rail.glyph()`), precomputed so
	// every app maps one `SourceGlyph → native asset` instead of switching
	// on `rail` itself (D5; `render-model.md` § Deltas). Web reads the
	// lowercase serde string off the snapshot; native apps match the enum.
	Glyph            fauna_core.SourceGlyph
	Flavor           ThreadFlavor
	Label            string
	Snippet          string
	LastActivityMs   int64
	UnreadCount      uint32
	ParticipantCount uint32
	// Which bridge carries this thread — `Some` only on a [`Rail::Bridged`]
	// thread whose bridge the rail's registry knows, `None` on every other
	// rail (`ui/conversations.md` § Where logic lives → *The `Bridged`
	// adapter*, ruling 2 (a)). When `Some`, [`Self::glyph`] is the bridge's
	// declared glyph and the label names the bridge; a bridged thread read
	// with no identity paints the generic `SourceGlyph::Bridge`. A
	// **read-time projection** filled by `ConversationsManager::snapshot`,
	// like [`ThreadDetail::room`].
	Bridge *fauna_core.BridgeIdentitySnapshot
	// The family gate's marker for this thread — see [`GuardianState`].
	// `Some` only on a [`Rail::Bridged`] thread of a supervised account whose
	// peer the nest reports held or blocked; a read-time projection from the
	// rail, like [`Self::bridge`].
	GuardianState *GuardianState
}

func (r *ThreadSummary) Destroy() {
	FfiDestroyerTypeThreadId{}.Destroy(r.ThreadId)
	FfiDestroyerRail{}.Destroy(r.Rail)
	fauna_core.FfiDestroyerSourceGlyph{}.Destroy(r.Glyph)
	FfiDestroyerThreadFlavor{}.Destroy(r.Flavor)
	FfiDestroyerString{}.Destroy(r.Label)
	FfiDestroyerString{}.Destroy(r.Snippet)
	FfiDestroyerInt64{}.Destroy(r.LastActivityMs)
	FfiDestroyerUint32{}.Destroy(r.UnreadCount)
	FfiDestroyerUint32{}.Destroy(r.ParticipantCount)
	FfiDestroyerOptionalBridgeIdentitySnapshot{}.Destroy(r.Bridge)
	FfiDestroyerOptionalGuardianState{}.Destroy(r.GuardianState)
}

type FfiConverterThreadSummary struct{}

var FfiConverterThreadSummaryINSTANCE = FfiConverterThreadSummary{}

func (c FfiConverterThreadSummary) Lift(rb RustBufferI) ThreadSummary {
	return LiftFromRustBuffer[ThreadSummary](c, rb)
}

func (c FfiConverterThreadSummary) Read(reader io.Reader) ThreadSummary {
	return ThreadSummary{
		FfiConverterTypeThreadIdINSTANCE.Read(reader),
		FfiConverterRailINSTANCE.Read(reader),
		fauna_core.FfiConverterSourceGlyphINSTANCE.Read(reader),
		FfiConverterThreadFlavorINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterOptionalBridgeIdentitySnapshotINSTANCE.Read(reader),
		FfiConverterOptionalGuardianStateINSTANCE.Read(reader),
	}
}

func (c FfiConverterThreadSummary) Lower(value ThreadSummary) C.RustBuffer {
	return LowerIntoRustBuffer[ThreadSummary](c, value)
}

func (c FfiConverterThreadSummary) LowerExternal(value ThreadSummary) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ThreadSummary](c, value))
}

func (c FfiConverterThreadSummary) Write(writer io.Writer, value ThreadSummary) {
	FfiConverterTypeThreadIdINSTANCE.Write(writer, value.ThreadId)
	FfiConverterRailINSTANCE.Write(writer, value.Rail)
	fauna_core.FfiConverterSourceGlyphINSTANCE.Write(writer, value.Glyph)
	FfiConverterThreadFlavorINSTANCE.Write(writer, value.Flavor)
	FfiConverterStringINSTANCE.Write(writer, value.Label)
	FfiConverterStringINSTANCE.Write(writer, value.Snippet)
	FfiConverterInt64INSTANCE.Write(writer, value.LastActivityMs)
	FfiConverterUint32INSTANCE.Write(writer, value.UnreadCount)
	FfiConverterUint32INSTANCE.Write(writer, value.ParticipantCount)
	FfiConverterOptionalBridgeIdentitySnapshotINSTANCE.Write(writer, value.Bridge)
	FfiConverterOptionalGuardianStateINSTANCE.Write(writer, value.GuardianState)
}

type FfiDestroyerThreadSummary struct{}

func (_ FfiDestroyerThreadSummary) Destroy(value ThreadSummary) {
	value.Destroy()
}

// One raised seat the cross-group eviction cannot clear from here — the
// channel named as hex (there is no `ThreadId`; that absence is what makes
// the seat unreachable) and the **typed** class, so every app renders its own
// localized remedy instead of shared Rust minting user-facing English.
type UnreachableSeat struct {
	ChannelHex string
	Class      UnreachableSeatClass
}

func (r *UnreachableSeat) Destroy() {
	FfiDestroyerString{}.Destroy(r.ChannelHex)
	FfiDestroyerUnreachableSeatClass{}.Destroy(r.Class)
}

type FfiConverterUnreachableSeat struct{}

var FfiConverterUnreachableSeatINSTANCE = FfiConverterUnreachableSeat{}

func (c FfiConverterUnreachableSeat) Lift(rb RustBufferI) UnreachableSeat {
	return LiftFromRustBuffer[UnreachableSeat](c, rb)
}

func (c FfiConverterUnreachableSeat) Read(reader io.Reader) UnreachableSeat {
	return UnreachableSeat{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUnreachableSeatClassINSTANCE.Read(reader),
	}
}

func (c FfiConverterUnreachableSeat) Lower(value UnreachableSeat) C.RustBuffer {
	return LowerIntoRustBuffer[UnreachableSeat](c, value)
}

func (c FfiConverterUnreachableSeat) LowerExternal(value UnreachableSeat) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[UnreachableSeat](c, value))
}

func (c FfiConverterUnreachableSeat) Write(writer io.Writer, value UnreachableSeat) {
	FfiConverterStringINSTANCE.Write(writer, value.ChannelHex)
	FfiConverterUnreachableSeatClassINSTANCE.Write(writer, value.Class)
}

type FfiDestroyerUnreachableSeat struct{}

func (_ FfiDestroyerUnreachableSeat) Destroy(value UnreachableSeat) {
	value.Destroy()
}

type BackendError struct {
	err error
}

// Convenience method to turn *BackendError into error
// Avoiding treating nil pointer as non nil error interface
func (err *BackendError) AsError() error {
	if err == nil {
		return nil
	} else {
		return err
	}
}

func (err BackendError) Error() string {
	return fmt.Sprintf("BackendError: %s", err.err.Error())
}

func (err BackendError) Unwrap() error {
	return err.err
}

// Err* are used for checking error type with `errors.Is`
var ErrBackendErrorNotSupported = fmt.Errorf("BackendErrorNotSupported")
var ErrBackendErrorTransport = fmt.Errorf("BackendErrorTransport")
var ErrBackendErrorNeedsUpdate = fmt.Errorf("BackendErrorNeedsUpdate")
var ErrBackendErrorAuthRequired = fmt.Errorf("BackendErrorAuthRequired")
var ErrBackendErrorRefusal = fmt.Errorf("BackendErrorRefusal")
var ErrBackendErrorInternal = fmt.Errorf("BackendErrorInternal")
var ErrBackendErrorWelcomeNotAddressedHere = fmt.Errorf("BackendErrorWelcomeNotAddressedHere")

// Variant structs
type BackendErrorNotSupported struct {
	message string
}

func NewBackendErrorNotSupported() *BackendError {
	return &BackendError{err: &BackendErrorNotSupported{}}
}

func (e BackendErrorNotSupported) destroy() {
}

func (err BackendErrorNotSupported) Error() string {
	return fmt.Sprintf("NotSupported: %s", err.message)
}

func (self BackendErrorNotSupported) Is(target error) bool {
	return target == ErrBackendErrorNotSupported
}

// A transport seam failed, carrying **that seam's** user-facing string (see
// [`SeamMessage`], which is also why this variant's payload cannot be built
// from an arbitrary error's `to_string()`).
type BackendErrorTransport struct {
	message string
}

// A transport seam failed, carrying **that seam's** user-facing string (see
// [`SeamMessage`], which is also why this variant's payload cannot be built
// from an arbitrary error's `to_string()`).
func NewBackendErrorTransport() *BackendError {
	return &BackendError{err: &BackendErrorTransport{}}
}

func (e BackendErrorTransport) destroy() {
}

func (err BackendErrorTransport) Error() string {
	return fmt.Sprintf("Transport: %s", err.message)
}

func (self BackendErrorTransport) Is(target error) bool {
	return target == ErrBackendErrorTransport
}

// The nest is running an **outdated version** (`fauna.nest.outdated`,
// `version-compatibility.md` Dimension 4) — distinct from
// [`Transport`](Self::Transport) so the conversations UI routes it to a
// non-retry "update your nest" affordance instead of an auto-retry, and
// renders this already-localized `message` rather than the raw transport
// `details`. Built from [`ConvRpcError::NeedsUpdate`] via the seam glue's
// shared `RpcError::action()`/`localized()` classifier (so every app
// shares the one mapping; priority #2). As a `uniffi(flat_error)` variant it
// crosses the FFI boundary by name (the field is dropped, but the `Display`
// — this `message` — is carried), so native apps catch the variant
// exactly as they catch `FfiError::NestOutdated` on the connect path.
type BackendErrorNeedsUpdate struct {
	message string
}

// The nest is running an **outdated version** (`fauna.nest.outdated`,
// `version-compatibility.md` Dimension 4) — distinct from
// [`Transport`](Self::Transport) so the conversations UI routes it to a
// non-retry "update your nest" affordance instead of an auto-retry, and
// renders this already-localized `message` rather than the raw transport
// `details`. Built from [`ConvRpcError::NeedsUpdate`] via the seam glue's
// shared `RpcError::action()`/`localized()` classifier (so every app
// shares the one mapping; priority #2). As a `uniffi(flat_error)` variant it
// crosses the FFI boundary by name (the field is dropped, but the `Display`
// — this `message` — is carried), so native apps catch the variant
// exactly as they catch `FfiError::NestOutdated` on the connect path.
func NewBackendErrorNeedsUpdate() *BackendError {
	return &BackendError{err: &BackendErrorNeedsUpdate{}}
}

func (e BackendErrorNeedsUpdate) destroy() {
}

func (err BackendErrorNeedsUpdate) Error() string {
	return fmt.Sprintf("NeedsUpdate: %s", err.message)
}

func (self BackendErrorNeedsUpdate) Is(target error) bool {
	return target == ErrBackendErrorNeedsUpdate
}

type BackendErrorAuthRequired struct {
	message string
}

func NewBackendErrorAuthRequired() *BackendError {
	return &BackendError{err: &BackendErrorAuthRequired{}}
}

func (e BackendErrorAuthRequired) destroy() {
}

func (err BackendErrorAuthRequired) Error() string {
	return fmt.Sprintf("AuthRequired: %s", err.message)
}

func (self BackendErrorAuthRequired) Is(target error) bool {
	return target == ErrBackendErrorAuthRequired
}

// A **product refusal** — the payload IS the user-facing sentence, drawn
// from the i18n table (`fauna_i18n::strings`, e.g. the inline-ceiling
// refusal's `error.email.too_large`) or already localized upstream
// (`ConvRpcError::Rejected`'s classified `message`). It rides `{message}`
// inside `conversations.unified.error_send` verbatim
// (`ConversationsManager::send` → [`SendState::failed`] via
// [`user_detail`](Self::user_detail)), so `conversations.md`
// § Architectural rules 3 ("never hardcode English") governs every
// producer: **constructing a `Refusal` from raw English is the bug this
// variant's name exists to make visible.** Its `Display` is the payload
// alone — no variant tag (a tag is hardcoded English in front of a
// localized sentence on all 7 apps at once, which is what the retired
// `Other`'s `"other: {0}"` did until 2026-07-30).
type BackendErrorRefusal struct {
	message string
}

// A **product refusal** — the payload IS the user-facing sentence, drawn
// from the i18n table (`fauna_i18n::strings`, e.g. the inline-ceiling
// refusal's `error.email.too_large`) or already localized upstream
// (`ConvRpcError::Rejected`'s classified `message`). It rides `{message}`
// inside `conversations.unified.error_send` verbatim
// (`ConversationsManager::send` → [`SendState::failed`] via
// [`user_detail`](Self::user_detail)), so `conversations.md`
// § Architectural rules 3 ("never hardcode English") governs every
// producer: **constructing a `Refusal` from raw English is the bug this
// variant's name exists to make visible.** Its `Display` is the payload
// alone — no variant tag (a tag is hardcoded English in front of a
// localized sentence on all 7 apps at once, which is what the retired
// `Other`'s `"other: {0}"` did until 2026-07-30).
func NewBackendErrorRefusal() *BackendError {
	return &BackendError{err: &BackendErrorRefusal{}}
}

func (e BackendErrorRefusal) destroy() {
}

func (err BackendErrorRefusal) Error() string {
	return fmt.Sprintf("Refusal: %s", err.message)
}

func (self BackendErrorRefusal) Is(target error) bool {
	return target == ErrBackendErrorRefusal
}

// A **developer diagnostic** — "no key package available for {actor}",
// "serialize welcome: {e:?}", an internal-signal string. The payload is
// **never rendered to the user**: [`user_detail`](Self::user_detail) maps
// this variant to the localized generic send failure
// (`error.send.generic`) and the raw payload belongs in the log
// (`{e}` / `{e:?}` at the catch site). Split from the retired catch-all
// `Other` (taxonomy ratified 2026-08-02, `conversations.md` § Errors &
// edge cases): one variant carried both product refusals and diagnostics,
// so ~30 raw internals reached `error-message` verbatim on all 7 apps.
type BackendErrorInternal struct {
	message string
}

// A **developer diagnostic** — "no key package available for {actor}",
// "serialize welcome: {e:?}", an internal-signal string. The payload is
// **never rendered to the user**: [`user_detail`](Self::user_detail) maps
// this variant to the localized generic send failure
// (`error.send.generic`) and the raw payload belongs in the log
// (`{e}` / `{e:?}` at the catch site). Split from the retired catch-all
// `Other` (taxonomy ratified 2026-08-02, `conversations.md` § Errors &
// edge cases): one variant carried both product refusals and diagnostics,
// so ~30 raw internals reached `error-message` verbatim on all 7 apps.
func NewBackendErrorInternal() *BackendError {
	return &BackendError{err: &BackendErrorInternal{}}
}

func (e BackendErrorInternal) destroy() {
}

func (err BackendErrorInternal) Error() string {
	return fmt.Sprintf("Internal: %s", err.message)
}

func (self BackendErrorInternal) Is(target error) bool {
	return target == ErrBackendErrorInternal
}

// A Welcome this device holds **no addressed key package** for — the
// multi-device steady state, never a fault
// (`docs/goal/behavior/devices.md` § Cross-device MLS group-state sync →
// *Who may consume a Welcome — any device that holds its key*). Carries
// [`fauna_mls::error::MlsError::NotAddressedToThisDevice`] across the
// backend seam, unchanged in meaning.
//
// Split from [`Internal`](Self::Internal) for the reason that variant was
// split from the retired `Other`: one variant carrying both a diagnostic
// and an expected outcome makes the two indistinguishable at every catch
// site. Here the catch site is the receive loop's push arm, which logged
// every device-not-addressed push at `error` — the same line a genuine
// ingest fault produces — on the long-open device of any two-device
// account. Producers/consumers may add nothing to that: the ingest is
// **not retried** and the row is **not acked**; the group reaches this
// device by the other door (the targeted sibling-group import) once the
// minting sibling's flush lands.
//
// Never rendered: like `Internal` it maps to the generic send sentence in
// [`user_detail`](Self::user_detail), and no send path can produce it.
type BackendErrorWelcomeNotAddressedHere struct {
	message string
}

// A Welcome this device holds **no addressed key package** for — the
// multi-device steady state, never a fault
// (`docs/goal/behavior/devices.md` § Cross-device MLS group-state sync →
// *Who may consume a Welcome — any device that holds its key*). Carries
// [`fauna_mls::error::MlsError::NotAddressedToThisDevice`] across the
// backend seam, unchanged in meaning.
//
// Split from [`Internal`](Self::Internal) for the reason that variant was
// split from the retired `Other`: one variant carrying both a diagnostic
// and an expected outcome makes the two indistinguishable at every catch
// site. Here the catch site is the receive loop's push arm, which logged
// every device-not-addressed push at `error` — the same line a genuine
// ingest fault produces — on the long-open device of any two-device
// account. Producers/consumers may add nothing to that: the ingest is
// **not retried** and the row is **not acked**; the group reaches this
// device by the other door (the targeted sibling-group import) once the
// minting sibling's flush lands.
//
// Never rendered: like `Internal` it maps to the generic send sentence in
// [`user_detail`](Self::user_detail), and no send path can produce it.
func NewBackendErrorWelcomeNotAddressedHere() *BackendError {
	return &BackendError{err: &BackendErrorWelcomeNotAddressedHere{}}
}

func (e BackendErrorWelcomeNotAddressedHere) destroy() {
}

func (err BackendErrorWelcomeNotAddressedHere) Error() string {
	return fmt.Sprintf("WelcomeNotAddressedHere: %s", err.message)
}

func (self BackendErrorWelcomeNotAddressedHere) Is(target error) bool {
	return target == ErrBackendErrorWelcomeNotAddressedHere
}

type FfiConverterBackendError struct{}

var FfiConverterBackendErrorINSTANCE = FfiConverterBackendError{}

func (c FfiConverterBackendError) Lift(eb RustBufferI) *BackendError {
	return LiftFromRustBuffer[*BackendError](c, eb)
}

func (c FfiConverterBackendError) Lower(value *BackendError) C.RustBuffer {
	return LowerIntoRustBuffer[*BackendError](c, value)
}

func (c FfiConverterBackendError) LowerExternal(value *BackendError) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*BackendError](c, value))
}

func (c FfiConverterBackendError) Read(reader io.Reader) *BackendError {
	errorID := readUint32(reader)

	message := FfiConverterStringINSTANCE.Read(reader)
	switch errorID {
	case 1:
		return &BackendError{&BackendErrorNotSupported{message}}
	case 2:
		return &BackendError{&BackendErrorTransport{message}}
	case 3:
		return &BackendError{&BackendErrorNeedsUpdate{message}}
	case 4:
		return &BackendError{&BackendErrorAuthRequired{message}}
	case 5:
		return &BackendError{&BackendErrorRefusal{message}}
	case 6:
		return &BackendError{&BackendErrorInternal{message}}
	case 7:
		return &BackendError{&BackendErrorWelcomeNotAddressedHere{message}}
	default:
		panic(fmt.Sprintf("Unknown error code %d in FfiConverterBackendError.Read()", errorID))
	}

}

func (c FfiConverterBackendError) Write(writer io.Writer, value *BackendError) {
	switch variantValue := value.err.(type) {
	case *BackendErrorNotSupported:
		writeInt32(writer, 1)
	case *BackendErrorTransport:
		writeInt32(writer, 2)
	case *BackendErrorNeedsUpdate:
		writeInt32(writer, 3)
	case *BackendErrorAuthRequired:
		writeInt32(writer, 4)
	case *BackendErrorRefusal:
		writeInt32(writer, 5)
	case *BackendErrorInternal:
		writeInt32(writer, 6)
	case *BackendErrorWelcomeNotAddressedHere:
		writeInt32(writer, 7)
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiConverterBackendError.Write", value))
	}
}

type FfiDestroyerBackendError struct{}

func (_ FfiDestroyerBackendError) Destroy(value *BackendError) {
	switch variantValue := value.err.(type) {
	case BackendErrorNotSupported:
		variantValue.destroy()
	case BackendErrorTransport:
		variantValue.destroy()
	case BackendErrorNeedsUpdate:
		variantValue.destroy()
	case BackendErrorAuthRequired:
		variantValue.destroy()
	case BackendErrorRefusal:
		variantValue.destroy()
	case BackendErrorInternal:
		variantValue.destroy()
	case BackendErrorWelcomeNotAddressedHere:
		variantValue.destroy()
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiDestroyerBackendError.Destroy", value))
	}
}

type BodyFormat uint

const (
	BodyFormatPlainText BodyFormat = 1
	BodyFormatMarkdown  BodyFormat = 2
	BodyFormatHtml      BodyFormat = 3
)

type FfiConverterBodyFormat struct{}

var FfiConverterBodyFormatINSTANCE = FfiConverterBodyFormat{}

func (c FfiConverterBodyFormat) Lift(rb RustBufferI) BodyFormat {
	return LiftFromRustBuffer[BodyFormat](c, rb)
}

func (c FfiConverterBodyFormat) Lower(value BodyFormat) C.RustBuffer {
	return LowerIntoRustBuffer[BodyFormat](c, value)
}

func (c FfiConverterBodyFormat) LowerExternal(value BodyFormat) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[BodyFormat](c, value))
}
func (FfiConverterBodyFormat) Read(reader io.Reader) BodyFormat {
	id := readInt32(reader)
	return BodyFormat(id)
}

func (FfiConverterBodyFormat) Write(writer io.Writer, value BodyFormat) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerBodyFormat struct{}

func (_ FfiDestroyerBodyFormat) Destroy(value BodyFormat) {
}

type DeliveryMode uint

const (
	DeliveryModeRealtime DeliveryMode = 1
	DeliveryModeAsync    DeliveryMode = 2
)

type FfiConverterDeliveryMode struct{}

var FfiConverterDeliveryModeINSTANCE = FfiConverterDeliveryMode{}

func (c FfiConverterDeliveryMode) Lift(rb RustBufferI) DeliveryMode {
	return LiftFromRustBuffer[DeliveryMode](c, rb)
}

func (c FfiConverterDeliveryMode) Lower(value DeliveryMode) C.RustBuffer {
	return LowerIntoRustBuffer[DeliveryMode](c, value)
}

func (c FfiConverterDeliveryMode) LowerExternal(value DeliveryMode) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[DeliveryMode](c, value))
}
func (FfiConverterDeliveryMode) Read(reader io.Reader) DeliveryMode {
	id := readInt32(reader)
	return DeliveryMode(id)
}

func (FfiConverterDeliveryMode) Write(writer io.Writer, value DeliveryMode) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerDeliveryMode struct{}

func (_ FfiDestroyerDeliveryMode) Destroy(value DeliveryMode) {
}

// The family gate's marker on a supervised account's bridged conversation —
// what `conversation-guardian-state` paints on the row and in the detail
// (`behavior/family-safety.md` § The bridge-DM gate). **Computed by the nest
// at every read and only painted here**: the user-side `rooms.list` row
// carries it, the bridged rail keeps the newest answer, and the manager
// projects it onto the thread. Absent for every unsupervised account. A read
// is never gated on it — a held thread stays fully readable.
type GuardianState uint

const (
	// The guardian reviews cold peers and has not decided this one.
	GuardianStateHeld GuardianState = 1
	// The guardian decided against this peer: its new messages never land
	// and a send to it is refused.
	GuardianStateBlocked GuardianState = 2
)

type FfiConverterGuardianState struct{}

var FfiConverterGuardianStateINSTANCE = FfiConverterGuardianState{}

func (c FfiConverterGuardianState) Lift(rb RustBufferI) GuardianState {
	return LiftFromRustBuffer[GuardianState](c, rb)
}

func (c FfiConverterGuardianState) Lower(value GuardianState) C.RustBuffer {
	return LowerIntoRustBuffer[GuardianState](c, value)
}

func (c FfiConverterGuardianState) LowerExternal(value GuardianState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[GuardianState](c, value))
}
func (FfiConverterGuardianState) Read(reader io.Reader) GuardianState {
	id := readInt32(reader)
	return GuardianState(id)
}

func (FfiConverterGuardianState) Write(writer io.Writer, value GuardianState) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerGuardianState struct{}

func (_ FfiDestroyerGuardianState) Destroy(value GuardianState) {
}

// What a newcomer sees of the room before their admission
// (`conversation-rooms.md` § History for joiners).
type HistoryPolicy uint

const (
	HistoryPolicyNone HistoryPolicy = 1
	HistoryPolicyFull HistoryPolicy = 2
)

type FfiConverterHistoryPolicy struct{}

var FfiConverterHistoryPolicyINSTANCE = FfiConverterHistoryPolicy{}

func (c FfiConverterHistoryPolicy) Lift(rb RustBufferI) HistoryPolicy {
	return LiftFromRustBuffer[HistoryPolicy](c, rb)
}

func (c FfiConverterHistoryPolicy) Lower(value HistoryPolicy) C.RustBuffer {
	return LowerIntoRustBuffer[HistoryPolicy](c, value)
}

func (c FfiConverterHistoryPolicy) LowerExternal(value HistoryPolicy) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[HistoryPolicy](c, value))
}
func (FfiConverterHistoryPolicy) Read(reader io.Reader) HistoryPolicy {
	id := readInt32(reader)
	return HistoryPolicy(id)
}

func (FfiConverterHistoryPolicy) Write(writer io.Writer, value HistoryPolicy) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerHistoryPolicy struct{}

func (_ FfiDestroyerHistoryPolicy) Destroy(value HistoryPolicy) {
}

// Who may invite (`conversation-rooms.md` § Join rules and invites).
type JoinRule uint

const (
	JoinRuleInvite       JoinRule = 1
	JoinRuleMemberInvite JoinRule = 2
	JoinRuleRequest      JoinRule = 3
)

type FfiConverterJoinRule struct{}

var FfiConverterJoinRuleINSTANCE = FfiConverterJoinRule{}

func (c FfiConverterJoinRule) Lift(rb RustBufferI) JoinRule {
	return LiftFromRustBuffer[JoinRule](c, rb)
}

func (c FfiConverterJoinRule) Lower(value JoinRule) C.RustBuffer {
	return LowerIntoRustBuffer[JoinRule](c, value)
}

func (c FfiConverterJoinRule) LowerExternal(value JoinRule) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[JoinRule](c, value))
}
func (FfiConverterJoinRule) Read(reader io.Reader) JoinRule {
	id := readInt32(reader)
	return JoinRule(id)
}

func (FfiConverterJoinRule) Write(writer io.Writer, value JoinRule) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerJoinRule struct{}

func (_ FfiDestroyerJoinRule) Destroy(value JoinRule) {
}

// What kind of principal a member is (`conversation-rooms.md` § The room →
// *Principals*). The class derives from the kinds alone.
type PrincipalKind uint

const (
	// A user — the actor and every device in their fleet, one entry.
	PrincipalKindUser PrincipalKind = 1
	// The room's home nest, holding a room-read key (community rooms only).
	PrincipalKindNest PrincipalKind = 2
	// A bridge principal or a mail transfer agent.
	PrincipalKindBridge PrincipalKind = 3
)

type FfiConverterPrincipalKind struct{}

var FfiConverterPrincipalKindINSTANCE = FfiConverterPrincipalKind{}

func (c FfiConverterPrincipalKind) Lift(rb RustBufferI) PrincipalKind {
	return LiftFromRustBuffer[PrincipalKind](c, rb)
}

func (c FfiConverterPrincipalKind) Lower(value PrincipalKind) C.RustBuffer {
	return LowerIntoRustBuffer[PrincipalKind](c, value)
}

func (c FfiConverterPrincipalKind) LowerExternal(value PrincipalKind) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[PrincipalKind](c, value))
}
func (FfiConverterPrincipalKind) Read(reader io.Reader) PrincipalKind {
	id := readInt32(reader)
	return PrincipalKind(id)
}

func (FfiConverterPrincipalKind) Write(writer io.Writer, value PrincipalKind) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerPrincipalKind struct{}

func (_ FfiDestroyerPrincipalKind) Destroy(value PrincipalKind) {
}

type Rail uint

const (
	RailFaunaMls Rail = 1
	RailSmtp     Rail = 2
	// Every bridge — one variant for all of them (`ui/conversations.md`
	// § Where logic lives → *The `Bridged` adapter*, ruling 2). A **unit**
	// variant: `Rail` stays `Copy` and backend registration stays keyed per
	// rail, so the per-bridge identity (id, label, glyph) rides the thread —
	// [`crate::snapshot::ThreadSummary::bridge`] — and the address —
	// [`TypedAddress::Bridged`] — never the rail.
	RailBridged Rail = 3
)

type FfiConverterRail struct{}

var FfiConverterRailINSTANCE = FfiConverterRail{}

func (c FfiConverterRail) Lift(rb RustBufferI) Rail {
	return LiftFromRustBuffer[Rail](c, rb)
}

func (c FfiConverterRail) Lower(value Rail) C.RustBuffer {
	return LowerIntoRustBuffer[Rail](c, value)
}

func (c FfiConverterRail) LowerExternal(value Rail) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[Rail](c, value))
}
func (FfiConverterRail) Read(reader io.Reader) Rail {
	id := readInt32(reader)
	return Rail(id)
}

func (FfiConverterRail) Write(writer io.Writer, value Rail) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerRail struct{}

func (_ FfiDestroyerRail) Destroy(value Rail) {
}

// Decoded by hand (below): a state a newer build writes reads as
// [`Self::Idle`].
type ResolveState uint

const (
	ResolveStateIdle      ResolveState = 1
	ResolveStateResolving ResolveState = 2
	ResolveStateResolved  ResolveState = 3
	// The address was probed but no rail claimed it — a syntactically-valid
	// recipient that isn't reachable (e.g. a Fauna actor id with no key
	// package on the nest). Distinct from `Error` (a transport/probe failure)
	// per `docs/goal/ui/conversations.md` § Errors & edge cases
	// (`recipient-resolve-status`: resolving / resolved / error / not-found).
	ResolveStateNotFound ResolveState = 4
	ResolveStateError    ResolveState = 5
)

type FfiConverterResolveState struct{}

var FfiConverterResolveStateINSTANCE = FfiConverterResolveState{}

func (c FfiConverterResolveState) Lift(rb RustBufferI) ResolveState {
	return LiftFromRustBuffer[ResolveState](c, rb)
}

func (c FfiConverterResolveState) Lower(value ResolveState) C.RustBuffer {
	return LowerIntoRustBuffer[ResolveState](c, value)
}

func (c FfiConverterResolveState) LowerExternal(value ResolveState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ResolveState](c, value))
}
func (FfiConverterResolveState) Read(reader io.Reader) ResolveState {
	id := readInt32(reader)
	return ResolveState(id)
}

func (FfiConverterResolveState) Write(writer io.Writer, value ResolveState) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerResolveState struct{}

func (_ FfiDestroyerResolveState) Destroy(value ResolveState) {
}

// A room's confidentiality class — **derived from its member set, never
// stored, never chosen** (`conversation-rooms.md` § The three classes).
type RoomClass uint

const (
	// Every member is a user's device fleet; the members read, nests relay
	// opaque bytes. MLS.
	RoomClassEndToEnd RoomClass = 1
	// The users plus the room's home nest, which reads under its room-read
	// key: moderatable, searchable, fanned out by the nest.
	RoomClassCommunity RoomClass = 2
	// A bridge or mail transfer agent is a member: the far network reads it
	// by construction, and the room says so honestly.
	RoomClassTransportOnly RoomClass = 3
)

type FfiConverterRoomClass struct{}

var FfiConverterRoomClassINSTANCE = FfiConverterRoomClass{}

func (c FfiConverterRoomClass) Lift(rb RustBufferI) RoomClass {
	return LiftFromRustBuffer[RoomClass](c, rb)
}

func (c FfiConverterRoomClass) Lower(value RoomClass) C.RustBuffer {
	return LowerIntoRustBuffer[RoomClass](c, value)
}

func (c FfiConverterRoomClass) LowerExternal(value RoomClass) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RoomClass](c, value))
}
func (FfiConverterRoomClass) Read(reader io.Reader) RoomClass {
	id := readInt32(reader)
	return RoomClass(id)
}

func (FfiConverterRoomClass) Write(writer io.Writer, value RoomClass) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerRoomClass struct{}

func (_ FfiDestroyerRoomClass) Destroy(value RoomClass) {
}

// The room's standing notice — what `thread-room-notice` states on the
// thread header while one holds (`ui/conversations.md` § Element IDs).
// Derived from two shared room facts, so no app re-decides which one speaks.
type RoomNotice uint

const (
	// A newcomer waiting to be keyed in ([`RoomSnapshot::awaiting_key`];
	// `community-rooms.md` § Implementation status today → *A newcomer's
	// walk waits for its key-in*).
	RoomNoticeAwaitingKey RoomNotice = 1
	// Some of the room's moderation could not be verified on this device
	// ([`RoomSnapshot::moderation_unverified`]; `conversation-rooms.md` §
	// Roles and authorization → *Members verify what they paint*).
	RoomNoticeModerationUnverified RoomNotice = 2
)

type FfiConverterRoomNotice struct{}

var FfiConverterRoomNoticeINSTANCE = FfiConverterRoomNotice{}

func (c FfiConverterRoomNotice) Lift(rb RustBufferI) RoomNotice {
	return LiftFromRustBuffer[RoomNotice](c, rb)
}

func (c FfiConverterRoomNotice) Lower(value RoomNotice) C.RustBuffer {
	return LowerIntoRustBuffer[RoomNotice](c, value)
}

func (c FfiConverterRoomNotice) LowerExternal(value RoomNotice) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RoomNotice](c, value))
}
func (FfiConverterRoomNotice) Read(reader io.Reader) RoomNotice {
	id := readInt32(reader)
	return RoomNotice(id)
}

func (FfiConverterRoomNotice) Write(writer io.Writer, value RoomNotice) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerRoomNotice struct{}

func (_ FfiDestroyerRoomNotice) Destroy(value RoomNotice) {
}

// A member's role (`conversation-rooms.md` § Roles and authorization).
type RoomRole uint

const (
	RoomRoleOwner  RoomRole = 1
	RoomRoleAdmin  RoomRole = 2
	RoomRoleMember RoomRole = 3
)

type FfiConverterRoomRole struct{}

var FfiConverterRoomRoleINSTANCE = FfiConverterRoomRole{}

func (c FfiConverterRoomRole) Lift(rb RustBufferI) RoomRole {
	return LiftFromRustBuffer[RoomRole](c, rb)
}

func (c FfiConverterRoomRole) Lower(value RoomRole) C.RustBuffer {
	return LowerIntoRustBuffer[RoomRole](c, value)
}

func (c FfiConverterRoomRole) LowerExternal(value RoomRole) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RoomRole](c, value))
}
func (FfiConverterRoomRole) Read(reader io.Reader) RoomRole {
	id := readInt32(reader)
	return RoomRole(id)
}

func (FfiConverterRoomRole) Write(writer io.Writer, value RoomRole) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerRoomRole struct{}

func (_ FfiDestroyerRoomRole) Destroy(value RoomRole) {
}

// One staged change of the room policy editor, in the manager's own terms.
//
// Ordered by [`RoomSettingsDraft::edits`]: the two rules first, then the
// appointments and demotions, then the home nest's read and the labeler set,
// then the hand-over **last** — after a hand-over lands this seat is a plain
// member and could commit none of the others.
type RoomSettingsEdit interface {
	Destroy()
}
type RoomSettingsEditJoinRule struct {
	Rule JoinRule
}

func (e RoomSettingsEditJoinRule) Destroy() {
	FfiDestroyerJoinRule{}.Destroy(e.Rule)
}

type RoomSettingsEditHistoryPolicy struct {
	Policy HistoryPolicy
}

func (e RoomSettingsEditHistoryPolicy) Destroy() {
	FfiDestroyerHistoryPolicy{}.Destroy(e.Policy)
}

type RoomSettingsEditAppoint struct {
	Address TypedAddress
}

func (e RoomSettingsEditAppoint) Destroy() {
	FfiDestroyerTypedAddress{}.Destroy(e.Address)
}

type RoomSettingsEditDemote struct {
	Address TypedAddress
}

func (e RoomSettingsEditDemote) Destroy() {
	FfiDestroyerTypedAddress{}.Destroy(e.Address)
}

// Grant or withdraw the home nest's read of a community room
// (`room-nest-read-toggle`) — a key **rotation**, not a policy commit
// (`conversation-rooms.md` § Implementation status today, the revoke).
// Before the hand-over, because a rotation needs the owner's or an
// admin's rank.
type RoomSettingsEditNestRead struct {
	Reads bool
}

func (e RoomSettingsEditNestRead) Destroy() {
	FfiDestroyerBool{}.Destroy(e.Reads)
}

// Replace a community room's labeler set with `labelers` — published
// labeler ids, lowercase hex (`conversation-rooms.md` § The three classes →
// *What the home nest does with its read*, purpose 2). One edit for the
// whole set: the set is one signed record, replaced whole. Before the
// hand-over, because naming what reads the room is the owner's or an
// admin's act.
type RoomSettingsEditLabelers struct {
	Labelers []string
}

func (e RoomSettingsEditLabelers) Destroy() {
	FfiDestroyerSequenceString{}.Destroy(e.Labelers)
}

// The hand-over (`conversation-rooms.md` § Roles and authorization →
// *Ownership transfer*) — always the last edit Save issues.
type RoomSettingsEditTransferOwnership struct {
	Address TypedAddress
}

func (e RoomSettingsEditTransferOwnership) Destroy() {
	FfiDestroyerTypedAddress{}.Destroy(e.Address)
}

type FfiConverterRoomSettingsEdit struct{}

var FfiConverterRoomSettingsEditINSTANCE = FfiConverterRoomSettingsEdit{}

func (c FfiConverterRoomSettingsEdit) Lift(rb RustBufferI) RoomSettingsEdit {
	return LiftFromRustBuffer[RoomSettingsEdit](c, rb)
}

func (c FfiConverterRoomSettingsEdit) Lower(value RoomSettingsEdit) C.RustBuffer {
	return LowerIntoRustBuffer[RoomSettingsEdit](c, value)
}

func (c FfiConverterRoomSettingsEdit) LowerExternal(value RoomSettingsEdit) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RoomSettingsEdit](c, value))
}
func (FfiConverterRoomSettingsEdit) Read(reader io.Reader) RoomSettingsEdit {
	id := readInt32(reader)
	switch id {
	case 1:
		return RoomSettingsEditJoinRule{
			FfiConverterJoinRuleINSTANCE.Read(reader),
		}
	case 2:
		return RoomSettingsEditHistoryPolicy{
			FfiConverterHistoryPolicyINSTANCE.Read(reader),
		}
	case 3:
		return RoomSettingsEditAppoint{
			FfiConverterTypedAddressINSTANCE.Read(reader),
		}
	case 4:
		return RoomSettingsEditDemote{
			FfiConverterTypedAddressINSTANCE.Read(reader),
		}
	case 5:
		return RoomSettingsEditNestRead{
			FfiConverterBoolINSTANCE.Read(reader),
		}
	case 6:
		return RoomSettingsEditLabelers{
			FfiConverterSequenceStringINSTANCE.Read(reader),
		}
	case 7:
		return RoomSettingsEditTransferOwnership{
			FfiConverterTypedAddressINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterRoomSettingsEdit.Read()", id))
	}
}

func (FfiConverterRoomSettingsEdit) Write(writer io.Writer, value RoomSettingsEdit) {
	switch variant_value := value.(type) {
	case RoomSettingsEditJoinRule:
		writeInt32(writer, 1)
		FfiConverterJoinRuleINSTANCE.Write(writer, variant_value.Rule)
	case RoomSettingsEditHistoryPolicy:
		writeInt32(writer, 2)
		FfiConverterHistoryPolicyINSTANCE.Write(writer, variant_value.Policy)
	case RoomSettingsEditAppoint:
		writeInt32(writer, 3)
		FfiConverterTypedAddressINSTANCE.Write(writer, variant_value.Address)
	case RoomSettingsEditDemote:
		writeInt32(writer, 4)
		FfiConverterTypedAddressINSTANCE.Write(writer, variant_value.Address)
	case RoomSettingsEditNestRead:
		writeInt32(writer, 5)
		FfiConverterBoolINSTANCE.Write(writer, variant_value.Reads)
	case RoomSettingsEditLabelers:
		writeInt32(writer, 6)
		FfiConverterSequenceStringINSTANCE.Write(writer, variant_value.Labelers)
	case RoomSettingsEditTransferOwnership:
		writeInt32(writer, 7)
		FfiConverterTypedAddressINSTANCE.Write(writer, variant_value.Address)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterRoomSettingsEdit.Write", value))
	}
}

type FfiDestroyerRoomSettingsEdit struct{}

func (_ FfiDestroyerRoomSettingsEdit) Destroy(value RoomSettingsEdit) {
	value.Destroy()
}

// Decoded by hand (below): a state a newer build writes reads as
// [`Self::Idle`].
type SendState interface {
	Destroy()
}
type SendStateIdle struct {
}

func (e SendStateIdle) Destroy() {
}

type SendStateSending struct {
}

func (e SendStateSending) Destroy() {
}

// A send the backend rejected. `reason` is a [`LocalizedText`] — the same
// carrier `ConversationsSnapshot::error` uses, because both feed the one
// `error-message` element and `conversations.md` § Architectural rules 3
// ("never hardcode English") governs the whole element, not one of its two
// producers. Always the key `conversations.unified.error_send` with the
// backend's own detail as `{message}`: sends have a single producer
// (`ConversationsManager::send`, which `send_new_thread` tail-calls), so
// unlike the page error there is no per-gesture key to choose.
type SendStateFailed struct {
	Reason fauna_core.LocalizedText
}

func (e SendStateFailed) Destroy() {
	fauna_core.FfiDestroyerLocalizedText{}.Destroy(e.Reason)
}

type FfiConverterSendState struct{}

var FfiConverterSendStateINSTANCE = FfiConverterSendState{}

func (c FfiConverterSendState) Lift(rb RustBufferI) SendState {
	return LiftFromRustBuffer[SendState](c, rb)
}

func (c FfiConverterSendState) Lower(value SendState) C.RustBuffer {
	return LowerIntoRustBuffer[SendState](c, value)
}

func (c FfiConverterSendState) LowerExternal(value SendState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[SendState](c, value))
}
func (FfiConverterSendState) Read(reader io.Reader) SendState {
	id := readInt32(reader)
	switch id {
	case 1:
		return SendStateIdle{}
	case 2:
		return SendStateSending{}
	case 3:
		return SendStateFailed{
			fauna_core.FfiConverterLocalizedTextINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterSendState.Read()", id))
	}
}

func (FfiConverterSendState) Write(writer io.Writer, value SendState) {
	switch variant_value := value.(type) {
	case SendStateIdle:
		writeInt32(writer, 1)
	case SendStateSending:
		writeInt32(writer, 2)
	case SendStateFailed:
		writeInt32(writer, 3)
		fauna_core.FfiConverterLocalizedTextINSTANCE.Write(writer, variant_value.Reason)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterSendState.Write", value))
	}
}

type FfiDestroyerSendState struct{}

func (_ FfiDestroyerSendState) Destroy(value SendState) {
	value.Destroy()
}

type SortOrder uint

const (
	SortOrderLatestActivity SortOrder = 1
	SortOrderOldestFirst    SortOrder = 2
	SortOrderUnread         SortOrder = 3
)

type FfiConverterSortOrder struct{}

var FfiConverterSortOrderINSTANCE = FfiConverterSortOrder{}

func (c FfiConverterSortOrder) Lift(rb RustBufferI) SortOrder {
	return LiftFromRustBuffer[SortOrder](c, rb)
}

func (c FfiConverterSortOrder) Lower(value SortOrder) C.RustBuffer {
	return LowerIntoRustBuffer[SortOrder](c, value)
}

func (c FfiConverterSortOrder) LowerExternal(value SortOrder) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[SortOrder](c, value))
}
func (FfiConverterSortOrder) Read(reader io.Reader) SortOrder {
	id := readInt32(reader)
	return SortOrder(id)
}

func (FfiConverterSortOrder) Write(writer io.Writer, value SortOrder) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerSortOrder struct{}

func (_ FfiDestroyerSortOrder) Destroy(value SortOrder) {
}

// Per-thread transport-crypto level — the **render of the room's class**
// (`conversation-rooms.md` § The three classes; [`crate::room::RoomClass`]).
// Distinct from `fauna_core::EncryptionMode` (the nest *storage* mode chosen
// at onboarding) — kept under a separate name so the two don't collide when
// both UniFFI namespaces are concatenated into one Swift module
// (`FaunaFFISwift`).
//
// One variant per class, and nothing else: the `None` arm the ActivityPub
// stub carried as a per-rail constant — a claim no roster produces — retired
// with that stub (`conversations.md` § Where logic lives → *The `Bridged`
// adapter*, ruling 3).
type ThreadEncryption uint

const (
	// End-to-end: the members read, nests relay opaque bytes.
	ThreadEncryptionE2e ThreadEncryption = 1
	// Transport-only: a bridge or mail transfer agent reads it by
	// construction.
	ThreadEncryptionTransportOnly ThreadEncryption = 2
	// Community: the members and the room's home nest, which reads under
	// its room-read key (`conversation-rooms.md` § The three classes →
	// *Community*).
	ThreadEncryptionNestReadable ThreadEncryption = 3
)

type FfiConverterThreadEncryption struct{}

var FfiConverterThreadEncryptionINSTANCE = FfiConverterThreadEncryption{}

func (c FfiConverterThreadEncryption) Lift(rb RustBufferI) ThreadEncryption {
	return LiftFromRustBuffer[ThreadEncryption](c, rb)
}

func (c FfiConverterThreadEncryption) Lower(value ThreadEncryption) C.RustBuffer {
	return LowerIntoRustBuffer[ThreadEncryption](c, value)
}

func (c FfiConverterThreadEncryption) LowerExternal(value ThreadEncryption) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ThreadEncryption](c, value))
}
func (FfiConverterThreadEncryption) Read(reader io.Reader) ThreadEncryption {
	id := readInt32(reader)
	return ThreadEncryption(id)
}

func (FfiConverterThreadEncryption) Write(writer io.Writer, value ThreadEncryption) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerThreadEncryption struct{}

func (_ FfiDestroyerThreadEncryption) Destroy(value ThreadEncryption) {
}

type ThreadFlavor interface {
	Destroy()
}
type ThreadFlavorOneToOne struct {
}

func (e ThreadFlavorOneToOne) Destroy() {
}

type ThreadFlavorMlsGroup struct {
}

func (e ThreadFlavorMlsGroup) Destroy() {
}

type ThreadFlavorSubjectKeyed struct {
}

func (e ThreadFlavorSubjectKeyed) Destroy() {
}

// A flavor a newer build writes and this one does not name, read out of
// a history slice another device wrote — carried whole as its canonical
// bytes so the slice this build merges and re-uploads keeps it
// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in
// full*; the shape: [`fauna_core::carried::canonical_bytes`]). The thread
// is shown, and behaves as the most restrictive known flavor: every
// flavor-gated affordance (group roster edits, subject threading) is
// withheld. No build writes one except by passing a carried value
// through.
type ThreadFlavorUnknown struct {
	Canonical []byte
}

func (e ThreadFlavorUnknown) Destroy() {
	FfiDestroyerBytes{}.Destroy(e.Canonical)
}

type FfiConverterThreadFlavor struct{}

var FfiConverterThreadFlavorINSTANCE = FfiConverterThreadFlavor{}

func (c FfiConverterThreadFlavor) Lift(rb RustBufferI) ThreadFlavor {
	return LiftFromRustBuffer[ThreadFlavor](c, rb)
}

func (c FfiConverterThreadFlavor) Lower(value ThreadFlavor) C.RustBuffer {
	return LowerIntoRustBuffer[ThreadFlavor](c, value)
}

func (c FfiConverterThreadFlavor) LowerExternal(value ThreadFlavor) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ThreadFlavor](c, value))
}
func (FfiConverterThreadFlavor) Read(reader io.Reader) ThreadFlavor {
	id := readInt32(reader)
	switch id {
	case 1:
		return ThreadFlavorOneToOne{}
	case 2:
		return ThreadFlavorMlsGroup{}
	case 3:
		return ThreadFlavorSubjectKeyed{}
	case 4:
		return ThreadFlavorUnknown{
			FfiConverterBytesINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterThreadFlavor.Read()", id))
	}
}

func (FfiConverterThreadFlavor) Write(writer io.Writer, value ThreadFlavor) {
	switch variant_value := value.(type) {
	case ThreadFlavorOneToOne:
		writeInt32(writer, 1)
	case ThreadFlavorMlsGroup:
		writeInt32(writer, 2)
	case ThreadFlavorSubjectKeyed:
		writeInt32(writer, 3)
	case ThreadFlavorUnknown:
		writeInt32(writer, 4)
		FfiConverterBytesINSTANCE.Write(writer, variant_value.Canonical)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterThreadFlavor.Write", value))
	}
}

type FfiDestroyerThreadFlavor struct{}

func (_ FfiDestroyerThreadFlavor) Destroy(value ThreadFlavor) {
	value.Destroy()
}

type TypedAddress interface {
	Destroy()
}
type TypedAddressFauna struct {
	Handle  string
	ActorId ActorId
}

func (e TypedAddressFauna) Destroy() {
	FfiDestroyerString{}.Destroy(e.Handle)
	FfiDestroyerTypeActorId{}.Destroy(e.ActorId)
}

type TypedAddressEmail struct {
	EmailAddress string
}

func (e TypedAddressEmail) Destroy() {
	FfiDestroyerString{}.Destroy(e.EmailAddress)
}

// An address on a far network a bridge serves — `bridge_id` is the
// bridge's manifest id, `address` the far network's own spelling of the
// participant (`ui/conversations.md` § Where logic lives → *The `Bridged`
// adapter*, ruling 2 (c)). Never parsed in an app: the bridge's address
// grammar is matched nest-side only (ruling 2 (d)).
type TypedAddressBridged struct {
	BridgeId string
	Address  string
}

func (e TypedAddressBridged) Destroy() {
	FfiDestroyerString{}.Destroy(e.BridgeId)
	FfiDestroyerString{}.Destroy(e.Address)
}

// An address of a kind a newer build writes and this one does not name —
// a participant or sender of a rail this build lacks, read out of a
// history slice or drafts blob another device wrote. Carried whole as
// its canonical bytes, so the slice this build merges and re-uploads
// keeps it byte-for-byte (`transport.md` § Schema and forward-compat
// discipline → *Rule 3 in full*; the shape:
// [`fauna_core::carried::canonical_bytes`]).
//
// It is shown, never acted on: it has no rail ([`Self::rail`] is `None`),
// so nothing sends to it, resolves it, or dials it; it names no person;
// it displays as a neutral placeholder. No build writes one except by
// passing a carried value through.
type TypedAddressUnknown struct {
	Canonical []byte
}

func (e TypedAddressUnknown) Destroy() {
	FfiDestroyerBytes{}.Destroy(e.Canonical)
}

type FfiConverterTypedAddress struct{}

var FfiConverterTypedAddressINSTANCE = FfiConverterTypedAddress{}

func (c FfiConverterTypedAddress) Lift(rb RustBufferI) TypedAddress {
	return LiftFromRustBuffer[TypedAddress](c, rb)
}

func (c FfiConverterTypedAddress) Lower(value TypedAddress) C.RustBuffer {
	return LowerIntoRustBuffer[TypedAddress](c, value)
}

func (c FfiConverterTypedAddress) LowerExternal(value TypedAddress) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[TypedAddress](c, value))
}
func (FfiConverterTypedAddress) Read(reader io.Reader) TypedAddress {
	id := readInt32(reader)
	switch id {
	case 1:
		return TypedAddressFauna{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterTypeActorIdINSTANCE.Read(reader),
		}
	case 2:
		return TypedAddressEmail{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 3:
		return TypedAddressBridged{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 4:
		return TypedAddressUnknown{
			FfiConverterBytesINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterTypedAddress.Read()", id))
	}
}

func (FfiConverterTypedAddress) Write(writer io.Writer, value TypedAddress) {
	switch variant_value := value.(type) {
	case TypedAddressFauna:
		writeInt32(writer, 1)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Handle)
		FfiConverterTypeActorIdINSTANCE.Write(writer, variant_value.ActorId)
	case TypedAddressEmail:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variant_value.EmailAddress)
	case TypedAddressBridged:
		writeInt32(writer, 3)
		FfiConverterStringINSTANCE.Write(writer, variant_value.BridgeId)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Address)
	case TypedAddressUnknown:
		writeInt32(writer, 4)
		FfiConverterBytesINSTANCE.Write(writer, variant_value.Canonical)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterTypedAddress.Write", value))
	}
}

type FfiDestroyerTypedAddress struct{}

func (_ FfiDestroyerTypedAddress) Destroy(value TypedAddress) {
	value.Destroy()
}

// Why a raised seat is not clearable from the review surface — § Propagation
// rule (5)'s two blocking classes plus the room policy's refusal, and
// deliberately only those three: a scheduling seat never blocks (no Commit is
// ever applied to a scheduling channel, so nothing can be planted there and
// nothing needs removing), so a variant for it here would be unrepresentable
// state.
type UnreachableSeatClass uint

const (
	// A governed room (`conversation-rooms.md` § Roles and authorization)
	// in which the viewer's role does not permit a Remove — a plain member
	// cannot evict anyone, and nobody evicts the owner. The remedy is the
	// room's owner or an admin acting from their own device; a Remove the
	// policy forbids is refused **before** any commit is authored, so it is
	// a typed fact here, never a silent skip or a retryable failure.
	UnreachableSeatClassNotPermittedByRoomPolicy UnreachableSeatClass = 1
	// A chat group of the owner's whose thread↔channel binding has not been
	// rebuilt on this device — removable once the thread syncs here, or from
	// a device that has it.
	UnreachableSeatClassChatGroupNoThreadHere UnreachableSeatClass = 2
	// A shared folder channel — removal is the set owner's key-rotating
	// remove in the folder's own sharing surface; a member of someone
	// else's set removes nobody but can leave the set. Either way the seat is
	// out of the raised span at the next press.
	UnreachableSeatClassFolderChannel UnreachableSeatClass = 3
)

type FfiConverterUnreachableSeatClass struct{}

var FfiConverterUnreachableSeatClassINSTANCE = FfiConverterUnreachableSeatClass{}

func (c FfiConverterUnreachableSeatClass) Lift(rb RustBufferI) UnreachableSeatClass {
	return LiftFromRustBuffer[UnreachableSeatClass](c, rb)
}

func (c FfiConverterUnreachableSeatClass) Lower(value UnreachableSeatClass) C.RustBuffer {
	return LowerIntoRustBuffer[UnreachableSeatClass](c, value)
}

func (c FfiConverterUnreachableSeatClass) LowerExternal(value UnreachableSeatClass) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[UnreachableSeatClass](c, value))
}
func (FfiConverterUnreachableSeatClass) Read(reader io.Reader) UnreachableSeatClass {
	id := readInt32(reader)
	return UnreachableSeatClass(id)
}

func (FfiConverterUnreachableSeatClass) Write(writer io.Writer, value UnreachableSeatClass) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerUnreachableSeatClass struct{}

func (_ FfiDestroyerUnreachableSeatClass) Destroy(value UnreachableSeatClass) {
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

type FfiConverterOptionalAddParticipantState struct{}

var FfiConverterOptionalAddParticipantStateINSTANCE = FfiConverterOptionalAddParticipantState{}

func (c FfiConverterOptionalAddParticipantState) Lift(rb RustBufferI) *AddParticipantState {
	return LiftFromRustBuffer[*AddParticipantState](c, rb)
}

func (_ FfiConverterOptionalAddParticipantState) Read(reader io.Reader) *AddParticipantState {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterAddParticipantStateINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalAddParticipantState) Lower(value *AddParticipantState) C.RustBuffer {
	return LowerIntoRustBuffer[*AddParticipantState](c, value)
}

func (c FfiConverterOptionalAddParticipantState) LowerExternal(value *AddParticipantState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*AddParticipantState](c, value))
}

func (_ FfiConverterOptionalAddParticipantState) Write(writer io.Writer, value *AddParticipantState) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterAddParticipantStateINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalAddParticipantState struct{}

func (_ FfiDestroyerOptionalAddParticipantState) Destroy(value *AddParticipantState) {
	if value != nil {
		FfiDestroyerAddParticipantState{}.Destroy(*value)
	}
}

type FfiConverterOptionalComposeState struct{}

var FfiConverterOptionalComposeStateINSTANCE = FfiConverterOptionalComposeState{}

func (c FfiConverterOptionalComposeState) Lift(rb RustBufferI) *ComposeState {
	return LiftFromRustBuffer[*ComposeState](c, rb)
}

func (_ FfiConverterOptionalComposeState) Read(reader io.Reader) *ComposeState {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterComposeStateINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalComposeState) Lower(value *ComposeState) C.RustBuffer {
	return LowerIntoRustBuffer[*ComposeState](c, value)
}

func (c FfiConverterOptionalComposeState) LowerExternal(value *ComposeState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*ComposeState](c, value))
}

func (_ FfiConverterOptionalComposeState) Write(writer io.Writer, value *ComposeState) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterComposeStateINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalComposeState struct{}

func (_ FfiDestroyerOptionalComposeState) Destroy(value *ComposeState) {
	if value != nil {
		FfiDestroyerComposeState{}.Destroy(*value)
	}
}

type FfiConverterOptionalListSendView struct{}

var FfiConverterOptionalListSendViewINSTANCE = FfiConverterOptionalListSendView{}

func (c FfiConverterOptionalListSendView) Lift(rb RustBufferI) *ListSendView {
	return LiftFromRustBuffer[*ListSendView](c, rb)
}

func (_ FfiConverterOptionalListSendView) Read(reader io.Reader) *ListSendView {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterListSendViewINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalListSendView) Lower(value *ListSendView) C.RustBuffer {
	return LowerIntoRustBuffer[*ListSendView](c, value)
}

func (c FfiConverterOptionalListSendView) LowerExternal(value *ListSendView) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*ListSendView](c, value))
}

func (_ FfiConverterOptionalListSendView) Write(writer io.Writer, value *ListSendView) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterListSendViewINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalListSendView struct{}

func (_ FfiDestroyerOptionalListSendView) Destroy(value *ListSendView) {
	if value != nil {
		FfiDestroyerListSendView{}.Destroy(*value)
	}
}

type FfiConverterOptionalPlaneRef struct{}

var FfiConverterOptionalPlaneRefINSTANCE = FfiConverterOptionalPlaneRef{}

func (c FfiConverterOptionalPlaneRef) Lift(rb RustBufferI) *PlaneRef {
	return LiftFromRustBuffer[*PlaneRef](c, rb)
}

func (_ FfiConverterOptionalPlaneRef) Read(reader io.Reader) *PlaneRef {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterPlaneRefINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalPlaneRef) Lower(value *PlaneRef) C.RustBuffer {
	return LowerIntoRustBuffer[*PlaneRef](c, value)
}

func (c FfiConverterOptionalPlaneRef) LowerExternal(value *PlaneRef) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*PlaneRef](c, value))
}

func (_ FfiConverterOptionalPlaneRef) Write(writer io.Writer, value *PlaneRef) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterPlaneRefINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalPlaneRef struct{}

func (_ FfiDestroyerOptionalPlaneRef) Destroy(value *PlaneRef) {
	if value != nil {
		FfiDestroyerPlaneRef{}.Destroy(*value)
	}
}

type FfiConverterOptionalRecipientPickerState struct{}

var FfiConverterOptionalRecipientPickerStateINSTANCE = FfiConverterOptionalRecipientPickerState{}

func (c FfiConverterOptionalRecipientPickerState) Lift(rb RustBufferI) *RecipientPickerState {
	return LiftFromRustBuffer[*RecipientPickerState](c, rb)
}

func (_ FfiConverterOptionalRecipientPickerState) Read(reader io.Reader) *RecipientPickerState {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterRecipientPickerStateINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalRecipientPickerState) Lower(value *RecipientPickerState) C.RustBuffer {
	return LowerIntoRustBuffer[*RecipientPickerState](c, value)
}

func (c FfiConverterOptionalRecipientPickerState) LowerExternal(value *RecipientPickerState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*RecipientPickerState](c, value))
}

func (_ FfiConverterOptionalRecipientPickerState) Write(writer io.Writer, value *RecipientPickerState) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterRecipientPickerStateINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalRecipientPickerState struct{}

func (_ FfiDestroyerOptionalRecipientPickerState) Destroy(value *RecipientPickerState) {
	if value != nil {
		FfiDestroyerRecipientPickerState{}.Destroy(*value)
	}
}

type FfiConverterOptionalReplyPreview struct{}

var FfiConverterOptionalReplyPreviewINSTANCE = FfiConverterOptionalReplyPreview{}

func (c FfiConverterOptionalReplyPreview) Lift(rb RustBufferI) *ReplyPreview {
	return LiftFromRustBuffer[*ReplyPreview](c, rb)
}

func (_ FfiConverterOptionalReplyPreview) Read(reader io.Reader) *ReplyPreview {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterReplyPreviewINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalReplyPreview) Lower(value *ReplyPreview) C.RustBuffer {
	return LowerIntoRustBuffer[*ReplyPreview](c, value)
}

func (c FfiConverterOptionalReplyPreview) LowerExternal(value *ReplyPreview) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*ReplyPreview](c, value))
}

func (_ FfiConverterOptionalReplyPreview) Write(writer io.Writer, value *ReplyPreview) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterReplyPreviewINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalReplyPreview struct{}

func (_ FfiDestroyerOptionalReplyPreview) Destroy(value *ReplyPreview) {
	if value != nil {
		FfiDestroyerReplyPreview{}.Destroy(*value)
	}
}

type FfiConverterOptionalRoomPolicySnapshot struct{}

var FfiConverterOptionalRoomPolicySnapshotINSTANCE = FfiConverterOptionalRoomPolicySnapshot{}

func (c FfiConverterOptionalRoomPolicySnapshot) Lift(rb RustBufferI) *RoomPolicySnapshot {
	return LiftFromRustBuffer[*RoomPolicySnapshot](c, rb)
}

func (_ FfiConverterOptionalRoomPolicySnapshot) Read(reader io.Reader) *RoomPolicySnapshot {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterRoomPolicySnapshotINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalRoomPolicySnapshot) Lower(value *RoomPolicySnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[*RoomPolicySnapshot](c, value)
}

func (c FfiConverterOptionalRoomPolicySnapshot) LowerExternal(value *RoomPolicySnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*RoomPolicySnapshot](c, value))
}

func (_ FfiConverterOptionalRoomPolicySnapshot) Write(writer io.Writer, value *RoomPolicySnapshot) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterRoomPolicySnapshotINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalRoomPolicySnapshot struct{}

func (_ FfiDestroyerOptionalRoomPolicySnapshot) Destroy(value *RoomPolicySnapshot) {
	if value != nil {
		FfiDestroyerRoomPolicySnapshot{}.Destroy(*value)
	}
}

type FfiConverterOptionalRoomSettingsDraft struct{}

var FfiConverterOptionalRoomSettingsDraftINSTANCE = FfiConverterOptionalRoomSettingsDraft{}

func (c FfiConverterOptionalRoomSettingsDraft) Lift(rb RustBufferI) *RoomSettingsDraft {
	return LiftFromRustBuffer[*RoomSettingsDraft](c, rb)
}

func (_ FfiConverterOptionalRoomSettingsDraft) Read(reader io.Reader) *RoomSettingsDraft {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterRoomSettingsDraftINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalRoomSettingsDraft) Lower(value *RoomSettingsDraft) C.RustBuffer {
	return LowerIntoRustBuffer[*RoomSettingsDraft](c, value)
}

func (c FfiConverterOptionalRoomSettingsDraft) LowerExternal(value *RoomSettingsDraft) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*RoomSettingsDraft](c, value))
}

func (_ FfiConverterOptionalRoomSettingsDraft) Write(writer io.Writer, value *RoomSettingsDraft) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterRoomSettingsDraftINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalRoomSettingsDraft struct{}

func (_ FfiDestroyerOptionalRoomSettingsDraft) Destroy(value *RoomSettingsDraft) {
	if value != nil {
		FfiDestroyerRoomSettingsDraft{}.Destroy(*value)
	}
}

type FfiConverterOptionalRoomSnapshot struct{}

var FfiConverterOptionalRoomSnapshotINSTANCE = FfiConverterOptionalRoomSnapshot{}

func (c FfiConverterOptionalRoomSnapshot) Lift(rb RustBufferI) *RoomSnapshot {
	return LiftFromRustBuffer[*RoomSnapshot](c, rb)
}

func (_ FfiConverterOptionalRoomSnapshot) Read(reader io.Reader) *RoomSnapshot {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterRoomSnapshotINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalRoomSnapshot) Lower(value *RoomSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[*RoomSnapshot](c, value)
}

func (c FfiConverterOptionalRoomSnapshot) LowerExternal(value *RoomSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*RoomSnapshot](c, value))
}

func (_ FfiConverterOptionalRoomSnapshot) Write(writer io.Writer, value *RoomSnapshot) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterRoomSnapshotINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalRoomSnapshot struct{}

func (_ FfiDestroyerOptionalRoomSnapshot) Destroy(value *RoomSnapshot) {
	if value != nil {
		FfiDestroyerRoomSnapshot{}.Destroy(*value)
	}
}

type FfiConverterOptionalThreadDetail struct{}

var FfiConverterOptionalThreadDetailINSTANCE = FfiConverterOptionalThreadDetail{}

func (c FfiConverterOptionalThreadDetail) Lift(rb RustBufferI) *ThreadDetail {
	return LiftFromRustBuffer[*ThreadDetail](c, rb)
}

func (_ FfiConverterOptionalThreadDetail) Read(reader io.Reader) *ThreadDetail {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterThreadDetailINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalThreadDetail) Lower(value *ThreadDetail) C.RustBuffer {
	return LowerIntoRustBuffer[*ThreadDetail](c, value)
}

func (c FfiConverterOptionalThreadDetail) LowerExternal(value *ThreadDetail) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*ThreadDetail](c, value))
}

func (_ FfiConverterOptionalThreadDetail) Write(writer io.Writer, value *ThreadDetail) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterThreadDetailINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalThreadDetail struct{}

func (_ FfiDestroyerOptionalThreadDetail) Destroy(value *ThreadDetail) {
	if value != nil {
		FfiDestroyerThreadDetail{}.Destroy(*value)
	}
}

type FfiConverterOptionalBridgeIdentitySnapshot struct{}

var FfiConverterOptionalBridgeIdentitySnapshotINSTANCE = FfiConverterOptionalBridgeIdentitySnapshot{}

func (c FfiConverterOptionalBridgeIdentitySnapshot) Lift(rb RustBufferI) *fauna_core.BridgeIdentitySnapshot {
	return LiftFromRustBuffer[*fauna_core.BridgeIdentitySnapshot](c, rb)
}

func (_ FfiConverterOptionalBridgeIdentitySnapshot) Read(reader io.Reader) *fauna_core.BridgeIdentitySnapshot {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := fauna_core.FfiConverterBridgeIdentitySnapshotINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalBridgeIdentitySnapshot) Lower(value *fauna_core.BridgeIdentitySnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[*fauna_core.BridgeIdentitySnapshot](c, value)
}

func (c FfiConverterOptionalBridgeIdentitySnapshot) LowerExternal(value *fauna_core.BridgeIdentitySnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*fauna_core.BridgeIdentitySnapshot](c, value))
}

func (_ FfiConverterOptionalBridgeIdentitySnapshot) Write(writer io.Writer, value *fauna_core.BridgeIdentitySnapshot) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		fauna_core.FfiConverterBridgeIdentitySnapshotINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalBridgeIdentitySnapshot struct{}

func (_ FfiDestroyerOptionalBridgeIdentitySnapshot) Destroy(value *fauna_core.BridgeIdentitySnapshot) {
	if value != nil {
		fauna_core.FfiDestroyerBridgeIdentitySnapshot{}.Destroy(*value)
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

type FfiConverterOptionalGuardianState struct{}

var FfiConverterOptionalGuardianStateINSTANCE = FfiConverterOptionalGuardianState{}

func (c FfiConverterOptionalGuardianState) Lift(rb RustBufferI) *GuardianState {
	return LiftFromRustBuffer[*GuardianState](c, rb)
}

func (_ FfiConverterOptionalGuardianState) Read(reader io.Reader) *GuardianState {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterGuardianStateINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalGuardianState) Lower(value *GuardianState) C.RustBuffer {
	return LowerIntoRustBuffer[*GuardianState](c, value)
}

func (c FfiConverterOptionalGuardianState) LowerExternal(value *GuardianState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*GuardianState](c, value))
}

func (_ FfiConverterOptionalGuardianState) Write(writer io.Writer, value *GuardianState) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterGuardianStateINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalGuardianState struct{}

func (_ FfiDestroyerOptionalGuardianState) Destroy(value *GuardianState) {
	if value != nil {
		FfiDestroyerGuardianState{}.Destroy(*value)
	}
}

type FfiConverterOptionalRoomClass struct{}

var FfiConverterOptionalRoomClassINSTANCE = FfiConverterOptionalRoomClass{}

func (c FfiConverterOptionalRoomClass) Lift(rb RustBufferI) *RoomClass {
	return LiftFromRustBuffer[*RoomClass](c, rb)
}

func (_ FfiConverterOptionalRoomClass) Read(reader io.Reader) *RoomClass {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterRoomClassINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalRoomClass) Lower(value *RoomClass) C.RustBuffer {
	return LowerIntoRustBuffer[*RoomClass](c, value)
}

func (c FfiConverterOptionalRoomClass) LowerExternal(value *RoomClass) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*RoomClass](c, value))
}

func (_ FfiConverterOptionalRoomClass) Write(writer io.Writer, value *RoomClass) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterRoomClassINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalRoomClass struct{}

func (_ FfiDestroyerOptionalRoomClass) Destroy(value *RoomClass) {
	if value != nil {
		FfiDestroyerRoomClass{}.Destroy(*value)
	}
}

type FfiConverterOptionalRoomNotice struct{}

var FfiConverterOptionalRoomNoticeINSTANCE = FfiConverterOptionalRoomNotice{}

func (c FfiConverterOptionalRoomNotice) Lift(rb RustBufferI) *RoomNotice {
	return LiftFromRustBuffer[*RoomNotice](c, rb)
}

func (_ FfiConverterOptionalRoomNotice) Read(reader io.Reader) *RoomNotice {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterRoomNoticeINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalRoomNotice) Lower(value *RoomNotice) C.RustBuffer {
	return LowerIntoRustBuffer[*RoomNotice](c, value)
}

func (c FfiConverterOptionalRoomNotice) LowerExternal(value *RoomNotice) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*RoomNotice](c, value))
}

func (_ FfiConverterOptionalRoomNotice) Write(writer io.Writer, value *RoomNotice) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterRoomNoticeINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalRoomNotice struct{}

func (_ FfiDestroyerOptionalRoomNotice) Destroy(value *RoomNotice) {
	if value != nil {
		FfiDestroyerRoomNotice{}.Destroy(*value)
	}
}

type FfiConverterOptionalRoomRole struct{}

var FfiConverterOptionalRoomRoleINSTANCE = FfiConverterOptionalRoomRole{}

func (c FfiConverterOptionalRoomRole) Lift(rb RustBufferI) *RoomRole {
	return LiftFromRustBuffer[*RoomRole](c, rb)
}

func (_ FfiConverterOptionalRoomRole) Read(reader io.Reader) *RoomRole {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterRoomRoleINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalRoomRole) Lower(value *RoomRole) C.RustBuffer {
	return LowerIntoRustBuffer[*RoomRole](c, value)
}

func (c FfiConverterOptionalRoomRole) LowerExternal(value *RoomRole) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*RoomRole](c, value))
}

func (_ FfiConverterOptionalRoomRole) Write(writer io.Writer, value *RoomRole) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterRoomRoleINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalRoomRole struct{}

func (_ FfiDestroyerOptionalRoomRole) Destroy(value *RoomRole) {
	if value != nil {
		FfiDestroyerRoomRole{}.Destroy(*value)
	}
}

type FfiConverterOptionalTypedAddress struct{}

var FfiConverterOptionalTypedAddressINSTANCE = FfiConverterOptionalTypedAddress{}

func (c FfiConverterOptionalTypedAddress) Lift(rb RustBufferI) *TypedAddress {
	return LiftFromRustBuffer[*TypedAddress](c, rb)
}

func (_ FfiConverterOptionalTypedAddress) Read(reader io.Reader) *TypedAddress {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterTypedAddressINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalTypedAddress) Lower(value *TypedAddress) C.RustBuffer {
	return LowerIntoRustBuffer[*TypedAddress](c, value)
}

func (c FfiConverterOptionalTypedAddress) LowerExternal(value *TypedAddress) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*TypedAddress](c, value))
}

func (_ FfiConverterOptionalTypedAddress) Write(writer io.Writer, value *TypedAddress) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterTypedAddressINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalTypedAddress struct{}

func (_ FfiDestroyerOptionalTypedAddress) Destroy(value *TypedAddress) {
	if value != nil {
		FfiDestroyerTypedAddress{}.Destroy(*value)
	}
}

type FfiConverterOptionalSequenceString struct{}

var FfiConverterOptionalSequenceStringINSTANCE = FfiConverterOptionalSequenceString{}

func (c FfiConverterOptionalSequenceString) Lift(rb RustBufferI) *[]string {
	return LiftFromRustBuffer[*[]string](c, rb)
}

func (_ FfiConverterOptionalSequenceString) Read(reader io.Reader) *[]string {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterSequenceStringINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalSequenceString) Lower(value *[]string) C.RustBuffer {
	return LowerIntoRustBuffer[*[]string](c, value)
}

func (c FfiConverterOptionalSequenceString) LowerExternal(value *[]string) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*[]string](c, value))
}

func (_ FfiConverterOptionalSequenceString) Write(writer io.Writer, value *[]string) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterSequenceStringINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalSequenceString struct{}

func (_ FfiDestroyerOptionalSequenceString) Destroy(value *[]string) {
	if value != nil {
		FfiDestroyerSequenceString{}.Destroy(*value)
	}
}

type FfiConverterOptionalSequenceRoomPendingInviteSnapshot struct{}

var FfiConverterOptionalSequenceRoomPendingInviteSnapshotINSTANCE = FfiConverterOptionalSequenceRoomPendingInviteSnapshot{}

func (c FfiConverterOptionalSequenceRoomPendingInviteSnapshot) Lift(rb RustBufferI) *[]RoomPendingInviteSnapshot {
	return LiftFromRustBuffer[*[]RoomPendingInviteSnapshot](c, rb)
}

func (_ FfiConverterOptionalSequenceRoomPendingInviteSnapshot) Read(reader io.Reader) *[]RoomPendingInviteSnapshot {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterSequenceRoomPendingInviteSnapshotINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalSequenceRoomPendingInviteSnapshot) Lower(value *[]RoomPendingInviteSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[*[]RoomPendingInviteSnapshot](c, value)
}

func (c FfiConverterOptionalSequenceRoomPendingInviteSnapshot) LowerExternal(value *[]RoomPendingInviteSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*[]RoomPendingInviteSnapshot](c, value))
}

func (_ FfiConverterOptionalSequenceRoomPendingInviteSnapshot) Write(writer io.Writer, value *[]RoomPendingInviteSnapshot) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterSequenceRoomPendingInviteSnapshotINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalSequenceRoomPendingInviteSnapshot struct{}

func (_ FfiDestroyerOptionalSequenceRoomPendingInviteSnapshot) Destroy(value *[]RoomPendingInviteSnapshot) {
	if value != nil {
		FfiDestroyerSequenceRoomPendingInviteSnapshot{}.Destroy(*value)
	}
}

type FfiConverterOptionalTypeMessageId struct{}

var FfiConverterOptionalTypeMessageIdINSTANCE = FfiConverterOptionalTypeMessageId{}

func (c FfiConverterOptionalTypeMessageId) Lift(rb RustBufferI) *MessageId {
	return LiftFromRustBuffer[*MessageId](c, rb)
}

func (_ FfiConverterOptionalTypeMessageId) Read(reader io.Reader) *MessageId {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterTypeMessageIdINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalTypeMessageId) Lower(value *MessageId) C.RustBuffer {
	return LowerIntoRustBuffer[*MessageId](c, value)
}

func (c FfiConverterOptionalTypeMessageId) LowerExternal(value *MessageId) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*MessageId](c, value))
}

func (_ FfiConverterOptionalTypeMessageId) Write(writer io.Writer, value *MessageId) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterTypeMessageIdINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalTypeMessageId struct{}

func (_ FfiDestroyerOptionalTypeMessageId) Destroy(value *MessageId) {
	if value != nil {
		FfiDestroyerTypeMessageId{}.Destroy(*value)
	}
}

type FfiConverterOptionalTypeThreadId struct{}

var FfiConverterOptionalTypeThreadIdINSTANCE = FfiConverterOptionalTypeThreadId{}

func (c FfiConverterOptionalTypeThreadId) Lift(rb RustBufferI) *ThreadId {
	return LiftFromRustBuffer[*ThreadId](c, rb)
}

func (_ FfiConverterOptionalTypeThreadId) Read(reader io.Reader) *ThreadId {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterTypeThreadIdINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalTypeThreadId) Lower(value *ThreadId) C.RustBuffer {
	return LowerIntoRustBuffer[*ThreadId](c, value)
}

func (c FfiConverterOptionalTypeThreadId) LowerExternal(value *ThreadId) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*ThreadId](c, value))
}

func (_ FfiConverterOptionalTypeThreadId) Write(writer io.Writer, value *ThreadId) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterTypeThreadIdINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalTypeThreadId struct{}

func (_ FfiDestroyerOptionalTypeThreadId) Destroy(value *ThreadId) {
	if value != nil {
		FfiDestroyerTypeThreadId{}.Destroy(*value)
	}
}

type FfiConverterSequenceBool struct{}

var FfiConverterSequenceBoolINSTANCE = FfiConverterSequenceBool{}

func (c FfiConverterSequenceBool) Lift(rb RustBufferI) []bool {
	return LiftFromRustBuffer[[]bool](c, rb)
}

func (c FfiConverterSequenceBool) Read(reader io.Reader) []bool {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]bool, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterBoolINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceBool) Lower(value []bool) C.RustBuffer {
	return LowerIntoRustBuffer[[]bool](c, value)
}

func (c FfiConverterSequenceBool) LowerExternal(value []bool) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]bool](c, value))
}

func (c FfiConverterSequenceBool) Write(writer io.Writer, value []bool) {
	if len(value) > math.MaxInt32 {
		panic("[]bool is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterBoolINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceBool struct{}

func (FfiDestroyerSequenceBool) Destroy(sequence []bool) {
	for _, value := range sequence {
		FfiDestroyerBool{}.Destroy(value)
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

type FfiConverterSequenceLocalDetection struct{}

var FfiConverterSequenceLocalDetectionINSTANCE = FfiConverterSequenceLocalDetection{}

func (c FfiConverterSequenceLocalDetection) Lift(rb RustBufferI) []fauna_client_moderation.LocalDetection {
	return LiftFromRustBuffer[[]fauna_client_moderation.LocalDetection](c, rb)
}

func (c FfiConverterSequenceLocalDetection) Read(reader io.Reader) []fauna_client_moderation.LocalDetection {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]fauna_client_moderation.LocalDetection, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, fauna_client_moderation.FfiConverterLocalDetectionINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceLocalDetection) Lower(value []fauna_client_moderation.LocalDetection) C.RustBuffer {
	return LowerIntoRustBuffer[[]fauna_client_moderation.LocalDetection](c, value)
}

func (c FfiConverterSequenceLocalDetection) LowerExternal(value []fauna_client_moderation.LocalDetection) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]fauna_client_moderation.LocalDetection](c, value))
}

func (c FfiConverterSequenceLocalDetection) Write(writer io.Writer, value []fauna_client_moderation.LocalDetection) {
	if len(value) > math.MaxInt32 {
		panic("[]fauna_client_moderation.LocalDetection is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		fauna_client_moderation.FfiConverterLocalDetectionINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceLocalDetection struct{}

func (FfiDestroyerSequenceLocalDetection) Destroy(sequence []fauna_client_moderation.LocalDetection) {
	for _, value := range sequence {
		fauna_client_moderation.FfiDestroyerLocalDetection{}.Destroy(value)
	}
}

type FfiConverterSequenceAttachmentDraft struct{}

var FfiConverterSequenceAttachmentDraftINSTANCE = FfiConverterSequenceAttachmentDraft{}

func (c FfiConverterSequenceAttachmentDraft) Lift(rb RustBufferI) []AttachmentDraft {
	return LiftFromRustBuffer[[]AttachmentDraft](c, rb)
}

func (c FfiConverterSequenceAttachmentDraft) Read(reader io.Reader) []AttachmentDraft {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]AttachmentDraft, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterAttachmentDraftINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceAttachmentDraft) Lower(value []AttachmentDraft) C.RustBuffer {
	return LowerIntoRustBuffer[[]AttachmentDraft](c, value)
}

func (c FfiConverterSequenceAttachmentDraft) LowerExternal(value []AttachmentDraft) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]AttachmentDraft](c, value))
}

func (c FfiConverterSequenceAttachmentDraft) Write(writer io.Writer, value []AttachmentDraft) {
	if len(value) > math.MaxInt32 {
		panic("[]AttachmentDraft is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterAttachmentDraftINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceAttachmentDraft struct{}

func (FfiDestroyerSequenceAttachmentDraft) Destroy(sequence []AttachmentDraft) {
	for _, value := range sequence {
		FfiDestroyerAttachmentDraft{}.Destroy(value)
	}
}

type FfiConverterSequenceAttachmentSnapshot struct{}

var FfiConverterSequenceAttachmentSnapshotINSTANCE = FfiConverterSequenceAttachmentSnapshot{}

func (c FfiConverterSequenceAttachmentSnapshot) Lift(rb RustBufferI) []AttachmentSnapshot {
	return LiftFromRustBuffer[[]AttachmentSnapshot](c, rb)
}

func (c FfiConverterSequenceAttachmentSnapshot) Read(reader io.Reader) []AttachmentSnapshot {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]AttachmentSnapshot, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterAttachmentSnapshotINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceAttachmentSnapshot) Lower(value []AttachmentSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[[]AttachmentSnapshot](c, value)
}

func (c FfiConverterSequenceAttachmentSnapshot) LowerExternal(value []AttachmentSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]AttachmentSnapshot](c, value))
}

func (c FfiConverterSequenceAttachmentSnapshot) Write(writer io.Writer, value []AttachmentSnapshot) {
	if len(value) > math.MaxInt32 {
		panic("[]AttachmentSnapshot is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterAttachmentSnapshotINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceAttachmentSnapshot struct{}

func (FfiDestroyerSequenceAttachmentSnapshot) Destroy(sequence []AttachmentSnapshot) {
	for _, value := range sequence {
		FfiDestroyerAttachmentSnapshot{}.Destroy(value)
	}
}

type FfiConverterSequenceEvictionFailure struct{}

var FfiConverterSequenceEvictionFailureINSTANCE = FfiConverterSequenceEvictionFailure{}

func (c FfiConverterSequenceEvictionFailure) Lift(rb RustBufferI) []EvictionFailure {
	return LiftFromRustBuffer[[]EvictionFailure](c, rb)
}

func (c FfiConverterSequenceEvictionFailure) Read(reader io.Reader) []EvictionFailure {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]EvictionFailure, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterEvictionFailureINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceEvictionFailure) Lower(value []EvictionFailure) C.RustBuffer {
	return LowerIntoRustBuffer[[]EvictionFailure](c, value)
}

func (c FfiConverterSequenceEvictionFailure) LowerExternal(value []EvictionFailure) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]EvictionFailure](c, value))
}

func (c FfiConverterSequenceEvictionFailure) Write(writer io.Writer, value []EvictionFailure) {
	if len(value) > math.MaxInt32 {
		panic("[]EvictionFailure is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterEvictionFailureINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceEvictionFailure struct{}

func (FfiDestroyerSequenceEvictionFailure) Destroy(sequence []EvictionFailure) {
	for _, value := range sequence {
		FfiDestroyerEvictionFailure{}.Destroy(value)
	}
}

type FfiConverterSequenceMessageSnapshot struct{}

var FfiConverterSequenceMessageSnapshotINSTANCE = FfiConverterSequenceMessageSnapshot{}

func (c FfiConverterSequenceMessageSnapshot) Lift(rb RustBufferI) []MessageSnapshot {
	return LiftFromRustBuffer[[]MessageSnapshot](c, rb)
}

func (c FfiConverterSequenceMessageSnapshot) Read(reader io.Reader) []MessageSnapshot {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]MessageSnapshot, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterMessageSnapshotINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceMessageSnapshot) Lower(value []MessageSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[[]MessageSnapshot](c, value)
}

func (c FfiConverterSequenceMessageSnapshot) LowerExternal(value []MessageSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]MessageSnapshot](c, value))
}

func (c FfiConverterSequenceMessageSnapshot) Write(writer io.Writer, value []MessageSnapshot) {
	if len(value) > math.MaxInt32 {
		panic("[]MessageSnapshot is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterMessageSnapshotINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceMessageSnapshot struct{}

func (FfiDestroyerSequenceMessageSnapshot) Destroy(sequence []MessageSnapshot) {
	for _, value := range sequence {
		FfiDestroyerMessageSnapshot{}.Destroy(value)
	}
}

type FfiConverterSequenceReactionGroup struct{}

var FfiConverterSequenceReactionGroupINSTANCE = FfiConverterSequenceReactionGroup{}

func (c FfiConverterSequenceReactionGroup) Lift(rb RustBufferI) []ReactionGroup {
	return LiftFromRustBuffer[[]ReactionGroup](c, rb)
}

func (c FfiConverterSequenceReactionGroup) Read(reader io.Reader) []ReactionGroup {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ReactionGroup, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterReactionGroupINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceReactionGroup) Lower(value []ReactionGroup) C.RustBuffer {
	return LowerIntoRustBuffer[[]ReactionGroup](c, value)
}

func (c FfiConverterSequenceReactionGroup) LowerExternal(value []ReactionGroup) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ReactionGroup](c, value))
}

func (c FfiConverterSequenceReactionGroup) Write(writer io.Writer, value []ReactionGroup) {
	if len(value) > math.MaxInt32 {
		panic("[]ReactionGroup is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterReactionGroupINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceReactionGroup struct{}

func (FfiDestroyerSequenceReactionGroup) Destroy(sequence []ReactionGroup) {
	for _, value := range sequence {
		FfiDestroyerReactionGroup{}.Destroy(value)
	}
}

type FfiConverterSequenceRoomInvitationSnapshot struct{}

var FfiConverterSequenceRoomInvitationSnapshotINSTANCE = FfiConverterSequenceRoomInvitationSnapshot{}

func (c FfiConverterSequenceRoomInvitationSnapshot) Lift(rb RustBufferI) []RoomInvitationSnapshot {
	return LiftFromRustBuffer[[]RoomInvitationSnapshot](c, rb)
}

func (c FfiConverterSequenceRoomInvitationSnapshot) Read(reader io.Reader) []RoomInvitationSnapshot {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]RoomInvitationSnapshot, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterRoomInvitationSnapshotINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceRoomInvitationSnapshot) Lower(value []RoomInvitationSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[[]RoomInvitationSnapshot](c, value)
}

func (c FfiConverterSequenceRoomInvitationSnapshot) LowerExternal(value []RoomInvitationSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]RoomInvitationSnapshot](c, value))
}

func (c FfiConverterSequenceRoomInvitationSnapshot) Write(writer io.Writer, value []RoomInvitationSnapshot) {
	if len(value) > math.MaxInt32 {
		panic("[]RoomInvitationSnapshot is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterRoomInvitationSnapshotINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceRoomInvitationSnapshot struct{}

func (FfiDestroyerSequenceRoomInvitationSnapshot) Destroy(sequence []RoomInvitationSnapshot) {
	for _, value := range sequence {
		FfiDestroyerRoomInvitationSnapshot{}.Destroy(value)
	}
}

type FfiConverterSequenceRoomMemberSnapshot struct{}

var FfiConverterSequenceRoomMemberSnapshotINSTANCE = FfiConverterSequenceRoomMemberSnapshot{}

func (c FfiConverterSequenceRoomMemberSnapshot) Lift(rb RustBufferI) []RoomMemberSnapshot {
	return LiftFromRustBuffer[[]RoomMemberSnapshot](c, rb)
}

func (c FfiConverterSequenceRoomMemberSnapshot) Read(reader io.Reader) []RoomMemberSnapshot {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]RoomMemberSnapshot, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterRoomMemberSnapshotINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceRoomMemberSnapshot) Lower(value []RoomMemberSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[[]RoomMemberSnapshot](c, value)
}

func (c FfiConverterSequenceRoomMemberSnapshot) LowerExternal(value []RoomMemberSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]RoomMemberSnapshot](c, value))
}

func (c FfiConverterSequenceRoomMemberSnapshot) Write(writer io.Writer, value []RoomMemberSnapshot) {
	if len(value) > math.MaxInt32 {
		panic("[]RoomMemberSnapshot is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterRoomMemberSnapshotINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceRoomMemberSnapshot struct{}

func (FfiDestroyerSequenceRoomMemberSnapshot) Destroy(sequence []RoomMemberSnapshot) {
	for _, value := range sequence {
		FfiDestroyerRoomMemberSnapshot{}.Destroy(value)
	}
}

type FfiConverterSequenceRoomPendingInviteSnapshot struct{}

var FfiConverterSequenceRoomPendingInviteSnapshotINSTANCE = FfiConverterSequenceRoomPendingInviteSnapshot{}

func (c FfiConverterSequenceRoomPendingInviteSnapshot) Lift(rb RustBufferI) []RoomPendingInviteSnapshot {
	return LiftFromRustBuffer[[]RoomPendingInviteSnapshot](c, rb)
}

func (c FfiConverterSequenceRoomPendingInviteSnapshot) Read(reader io.Reader) []RoomPendingInviteSnapshot {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]RoomPendingInviteSnapshot, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterRoomPendingInviteSnapshotINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceRoomPendingInviteSnapshot) Lower(value []RoomPendingInviteSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[[]RoomPendingInviteSnapshot](c, value)
}

func (c FfiConverterSequenceRoomPendingInviteSnapshot) LowerExternal(value []RoomPendingInviteSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]RoomPendingInviteSnapshot](c, value))
}

func (c FfiConverterSequenceRoomPendingInviteSnapshot) Write(writer io.Writer, value []RoomPendingInviteSnapshot) {
	if len(value) > math.MaxInt32 {
		panic("[]RoomPendingInviteSnapshot is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterRoomPendingInviteSnapshotINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceRoomPendingInviteSnapshot struct{}

func (FfiDestroyerSequenceRoomPendingInviteSnapshot) Destroy(sequence []RoomPendingInviteSnapshot) {
	for _, value := range sequence {
		FfiDestroyerRoomPendingInviteSnapshot{}.Destroy(value)
	}
}

type FfiConverterSequenceThreadSummary struct{}

var FfiConverterSequenceThreadSummaryINSTANCE = FfiConverterSequenceThreadSummary{}

func (c FfiConverterSequenceThreadSummary) Lift(rb RustBufferI) []ThreadSummary {
	return LiftFromRustBuffer[[]ThreadSummary](c, rb)
}

func (c FfiConverterSequenceThreadSummary) Read(reader io.Reader) []ThreadSummary {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ThreadSummary, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterThreadSummaryINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceThreadSummary) Lower(value []ThreadSummary) C.RustBuffer {
	return LowerIntoRustBuffer[[]ThreadSummary](c, value)
}

func (c FfiConverterSequenceThreadSummary) LowerExternal(value []ThreadSummary) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ThreadSummary](c, value))
}

func (c FfiConverterSequenceThreadSummary) Write(writer io.Writer, value []ThreadSummary) {
	if len(value) > math.MaxInt32 {
		panic("[]ThreadSummary is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterThreadSummaryINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceThreadSummary struct{}

func (FfiDestroyerSequenceThreadSummary) Destroy(sequence []ThreadSummary) {
	for _, value := range sequence {
		FfiDestroyerThreadSummary{}.Destroy(value)
	}
}

type FfiConverterSequenceUnreachableSeat struct{}

var FfiConverterSequenceUnreachableSeatINSTANCE = FfiConverterSequenceUnreachableSeat{}

func (c FfiConverterSequenceUnreachableSeat) Lift(rb RustBufferI) []UnreachableSeat {
	return LiftFromRustBuffer[[]UnreachableSeat](c, rb)
}

func (c FfiConverterSequenceUnreachableSeat) Read(reader io.Reader) []UnreachableSeat {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]UnreachableSeat, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterUnreachableSeatINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceUnreachableSeat) Lower(value []UnreachableSeat) C.RustBuffer {
	return LowerIntoRustBuffer[[]UnreachableSeat](c, value)
}

func (c FfiConverterSequenceUnreachableSeat) LowerExternal(value []UnreachableSeat) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]UnreachableSeat](c, value))
}

func (c FfiConverterSequenceUnreachableSeat) Write(writer io.Writer, value []UnreachableSeat) {
	if len(value) > math.MaxInt32 {
		panic("[]UnreachableSeat is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterUnreachableSeatINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceUnreachableSeat struct{}

func (FfiDestroyerSequenceUnreachableSeat) Destroy(sequence []UnreachableSeat) {
	for _, value := range sequence {
		FfiDestroyerUnreachableSeat{}.Destroy(value)
	}
}

type FfiConverterSequenceBridgeIdentitySnapshot struct{}

var FfiConverterSequenceBridgeIdentitySnapshotINSTANCE = FfiConverterSequenceBridgeIdentitySnapshot{}

func (c FfiConverterSequenceBridgeIdentitySnapshot) Lift(rb RustBufferI) []fauna_core.BridgeIdentitySnapshot {
	return LiftFromRustBuffer[[]fauna_core.BridgeIdentitySnapshot](c, rb)
}

func (c FfiConverterSequenceBridgeIdentitySnapshot) Read(reader io.Reader) []fauna_core.BridgeIdentitySnapshot {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]fauna_core.BridgeIdentitySnapshot, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, fauna_core.FfiConverterBridgeIdentitySnapshotINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceBridgeIdentitySnapshot) Lower(value []fauna_core.BridgeIdentitySnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[[]fauna_core.BridgeIdentitySnapshot](c, value)
}

func (c FfiConverterSequenceBridgeIdentitySnapshot) LowerExternal(value []fauna_core.BridgeIdentitySnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]fauna_core.BridgeIdentitySnapshot](c, value))
}

func (c FfiConverterSequenceBridgeIdentitySnapshot) Write(writer io.Writer, value []fauna_core.BridgeIdentitySnapshot) {
	if len(value) > math.MaxInt32 {
		panic("[]fauna_core.BridgeIdentitySnapshot is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		fauna_core.FfiConverterBridgeIdentitySnapshotINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceBridgeIdentitySnapshot struct{}

func (FfiDestroyerSequenceBridgeIdentitySnapshot) Destroy(sequence []fauna_core.BridgeIdentitySnapshot) {
	for _, value := range sequence {
		fauna_core.FfiDestroyerBridgeIdentitySnapshot{}.Destroy(value)
	}
}

type FfiConverterSequenceContentLabelEntry struct{}

var FfiConverterSequenceContentLabelEntryINSTANCE = FfiConverterSequenceContentLabelEntry{}

func (c FfiConverterSequenceContentLabelEntry) Lift(rb RustBufferI) []fauna_core.ContentLabelEntry {
	return LiftFromRustBuffer[[]fauna_core.ContentLabelEntry](c, rb)
}

func (c FfiConverterSequenceContentLabelEntry) Read(reader io.Reader) []fauna_core.ContentLabelEntry {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]fauna_core.ContentLabelEntry, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, fauna_core.FfiConverterContentLabelEntryINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceContentLabelEntry) Lower(value []fauna_core.ContentLabelEntry) C.RustBuffer {
	return LowerIntoRustBuffer[[]fauna_core.ContentLabelEntry](c, value)
}

func (c FfiConverterSequenceContentLabelEntry) LowerExternal(value []fauna_core.ContentLabelEntry) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]fauna_core.ContentLabelEntry](c, value))
}

func (c FfiConverterSequenceContentLabelEntry) Write(writer io.Writer, value []fauna_core.ContentLabelEntry) {
	if len(value) > math.MaxInt32 {
		panic("[]fauna_core.ContentLabelEntry is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		fauna_core.FfiConverterContentLabelEntryINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceContentLabelEntry struct{}

func (FfiDestroyerSequenceContentLabelEntry) Destroy(sequence []fauna_core.ContentLabelEntry) {
	for _, value := range sequence {
		fauna_core.FfiDestroyerContentLabelEntry{}.Destroy(value)
	}
}

type FfiConverterSequenceHistoryPolicy struct{}

var FfiConverterSequenceHistoryPolicyINSTANCE = FfiConverterSequenceHistoryPolicy{}

func (c FfiConverterSequenceHistoryPolicy) Lift(rb RustBufferI) []HistoryPolicy {
	return LiftFromRustBuffer[[]HistoryPolicy](c, rb)
}

func (c FfiConverterSequenceHistoryPolicy) Read(reader io.Reader) []HistoryPolicy {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]HistoryPolicy, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterHistoryPolicyINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceHistoryPolicy) Lower(value []HistoryPolicy) C.RustBuffer {
	return LowerIntoRustBuffer[[]HistoryPolicy](c, value)
}

func (c FfiConverterSequenceHistoryPolicy) LowerExternal(value []HistoryPolicy) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]HistoryPolicy](c, value))
}

func (c FfiConverterSequenceHistoryPolicy) Write(writer io.Writer, value []HistoryPolicy) {
	if len(value) > math.MaxInt32 {
		panic("[]HistoryPolicy is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterHistoryPolicyINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceHistoryPolicy struct{}

func (FfiDestroyerSequenceHistoryPolicy) Destroy(sequence []HistoryPolicy) {
	for _, value := range sequence {
		FfiDestroyerHistoryPolicy{}.Destroy(value)
	}
}

type FfiConverterSequenceJoinRule struct{}

var FfiConverterSequenceJoinRuleINSTANCE = FfiConverterSequenceJoinRule{}

func (c FfiConverterSequenceJoinRule) Lift(rb RustBufferI) []JoinRule {
	return LiftFromRustBuffer[[]JoinRule](c, rb)
}

func (c FfiConverterSequenceJoinRule) Read(reader io.Reader) []JoinRule {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]JoinRule, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterJoinRuleINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceJoinRule) Lower(value []JoinRule) C.RustBuffer {
	return LowerIntoRustBuffer[[]JoinRule](c, value)
}

func (c FfiConverterSequenceJoinRule) LowerExternal(value []JoinRule) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]JoinRule](c, value))
}

func (c FfiConverterSequenceJoinRule) Write(writer io.Writer, value []JoinRule) {
	if len(value) > math.MaxInt32 {
		panic("[]JoinRule is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterJoinRuleINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceJoinRule struct{}

func (FfiDestroyerSequenceJoinRule) Destroy(sequence []JoinRule) {
	for _, value := range sequence {
		FfiDestroyerJoinRule{}.Destroy(value)
	}
}

type FfiConverterSequenceRoomSettingsEdit struct{}

var FfiConverterSequenceRoomSettingsEditINSTANCE = FfiConverterSequenceRoomSettingsEdit{}

func (c FfiConverterSequenceRoomSettingsEdit) Lift(rb RustBufferI) []RoomSettingsEdit {
	return LiftFromRustBuffer[[]RoomSettingsEdit](c, rb)
}

func (c FfiConverterSequenceRoomSettingsEdit) Read(reader io.Reader) []RoomSettingsEdit {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]RoomSettingsEdit, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterRoomSettingsEditINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceRoomSettingsEdit) Lower(value []RoomSettingsEdit) C.RustBuffer {
	return LowerIntoRustBuffer[[]RoomSettingsEdit](c, value)
}

func (c FfiConverterSequenceRoomSettingsEdit) LowerExternal(value []RoomSettingsEdit) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]RoomSettingsEdit](c, value))
}

func (c FfiConverterSequenceRoomSettingsEdit) Write(writer io.Writer, value []RoomSettingsEdit) {
	if len(value) > math.MaxInt32 {
		panic("[]RoomSettingsEdit is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterRoomSettingsEditINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceRoomSettingsEdit struct{}

func (FfiDestroyerSequenceRoomSettingsEdit) Destroy(sequence []RoomSettingsEdit) {
	for _, value := range sequence {
		FfiDestroyerRoomSettingsEdit{}.Destroy(value)
	}
}

type FfiConverterSequenceTypedAddress struct{}

var FfiConverterSequenceTypedAddressINSTANCE = FfiConverterSequenceTypedAddress{}

func (c FfiConverterSequenceTypedAddress) Lift(rb RustBufferI) []TypedAddress {
	return LiftFromRustBuffer[[]TypedAddress](c, rb)
}

func (c FfiConverterSequenceTypedAddress) Read(reader io.Reader) []TypedAddress {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]TypedAddress, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterTypedAddressINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceTypedAddress) Lower(value []TypedAddress) C.RustBuffer {
	return LowerIntoRustBuffer[[]TypedAddress](c, value)
}

func (c FfiConverterSequenceTypedAddress) LowerExternal(value []TypedAddress) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]TypedAddress](c, value))
}

func (c FfiConverterSequenceTypedAddress) Write(writer io.Writer, value []TypedAddress) {
	if len(value) > math.MaxInt32 {
		panic("[]TypedAddress is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterTypedAddressINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceTypedAddress struct{}

func (FfiDestroyerSequenceTypedAddress) Destroy(sequence []TypedAddress) {
	for _, value := range sequence {
		FfiDestroyerTypedAddress{}.Destroy(value)
	}
}

type FfiConverterSequenceTypeActorId struct{}

var FfiConverterSequenceTypeActorIdINSTANCE = FfiConverterSequenceTypeActorId{}

func (c FfiConverterSequenceTypeActorId) Lift(rb RustBufferI) []ActorId {
	return LiftFromRustBuffer[[]ActorId](c, rb)
}

func (c FfiConverterSequenceTypeActorId) Read(reader io.Reader) []ActorId {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ActorId, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterTypeActorIdINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceTypeActorId) Lower(value []ActorId) C.RustBuffer {
	return LowerIntoRustBuffer[[]ActorId](c, value)
}

func (c FfiConverterSequenceTypeActorId) LowerExternal(value []ActorId) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ActorId](c, value))
}

func (c FfiConverterSequenceTypeActorId) Write(writer io.Writer, value []ActorId) {
	if len(value) > math.MaxInt32 {
		panic("[]ActorId is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterTypeActorIdINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceTypeActorId struct{}

func (FfiDestroyerSequenceTypeActorId) Destroy(sequence []ActorId) {
	for _, value := range sequence {
		FfiDestroyerTypeActorId{}.Destroy(value)
	}
}

type FfiConverterSequenceTypeThreadId struct{}

var FfiConverterSequenceTypeThreadIdINSTANCE = FfiConverterSequenceTypeThreadId{}

func (c FfiConverterSequenceTypeThreadId) Lift(rb RustBufferI) []ThreadId {
	return LiftFromRustBuffer[[]ThreadId](c, rb)
}

func (c FfiConverterSequenceTypeThreadId) Read(reader io.Reader) []ThreadId {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ThreadId, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterTypeThreadIdINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceTypeThreadId) Lower(value []ThreadId) C.RustBuffer {
	return LowerIntoRustBuffer[[]ThreadId](c, value)
}

func (c FfiConverterSequenceTypeThreadId) LowerExternal(value []ThreadId) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ThreadId](c, value))
}

func (c FfiConverterSequenceTypeThreadId) Write(writer io.Writer, value []ThreadId) {
	if len(value) > math.MaxInt32 {
		panic("[]ThreadId is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterTypeThreadIdINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceTypeThreadId struct{}

func (FfiDestroyerSequenceTypeThreadId) Destroy(sequence []ThreadId) {
	for _, value := range sequence {
		FfiDestroyerTypeThreadId{}.Destroy(value)
	}
}

/**
 * Typealias from the type name used in the UDL file to the builtin type.  This
 * is needed because the UDL type name is used in function/method signatures.
 * It's also what we have an external type that references a custom type.
 */
type ActorId = []byte
type FfiConverterTypeActorId = FfiConverterBytes
type FfiDestroyerTypeActorId = FfiDestroyerBytes

var FfiConverterTypeActorIdINSTANCE = FfiConverterBytes{}

func LiftFromExternalTypeActorId(value ExternalCRustBuffer) ActorId {
	return FfiConverterTypeActorIdINSTANCE.Lift(RustBufferFromExternal(value))
}

func LowerToExternalTypeActorId(value ActorId) ExternalCRustBuffer {
	return RustBufferFromC(FfiConverterTypeActorIdINSTANCE.Lower(value))
}

/**
 * Typealias from the type name used in the UDL file to the builtin type.  This
 * is needed because the UDL type name is used in function/method signatures.
 * It's also what we have an external type that references a custom type.
 */
type MessageId = string
type FfiConverterTypeMessageId = FfiConverterString
type FfiDestroyerTypeMessageId = FfiDestroyerString

var FfiConverterTypeMessageIdINSTANCE = FfiConverterString{}

func LiftFromExternalTypeMessageId(value ExternalCRustBuffer) MessageId {
	return FfiConverterTypeMessageIdINSTANCE.Lift(RustBufferFromExternal(value))
}

func LowerToExternalTypeMessageId(value MessageId) ExternalCRustBuffer {
	return RustBufferFromC(FfiConverterTypeMessageIdINSTANCE.Lower(value))
}

/**
 * Typealias from the type name used in the UDL file to the builtin type.  This
 * is needed because the UDL type name is used in function/method signatures.
 * It's also what we have an external type that references a custom type.
 */
type ThreadId = string
type FfiConverterTypeThreadId = FfiConverterString
type FfiDestroyerTypeThreadId = FfiDestroyerString

var FfiConverterTypeThreadIdINSTANCE = FfiConverterString{}

func LiftFromExternalTypeThreadId(value ExternalCRustBuffer) ThreadId {
	return FfiConverterTypeThreadIdINSTANCE.Lift(RustBufferFromExternal(value))
}

func LowerToExternalTypeThreadId(value ThreadId) ExternalCRustBuffer {
	return RustBufferFromC(FfiConverterTypeThreadIdINSTANCE.Lower(value))
}

const (
	uniffiRustFuturePollReady      int8 = 0
	uniffiRustFuturePollMaybeReady int8 = 1
)

type rustFuturePollFunc func(C.uint64_t, C.UniffiRustFutureContinuationCallback, C.uint64_t)
type rustFutureCompleteFunc[T any] func(C.uint64_t, *C.RustCallStatus) T
type rustFutureFreeFunc func(C.uint64_t)

//export fauna_conversations_uniffiFutureContinuationCallback
func fauna_conversations_uniffiFutureContinuationCallback(data C.uint64_t, pollResult C.int8_t) {
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
			(C.UniffiRustFutureContinuationCallback)(C.fauna_conversations_uniffiFutureContinuationCallback),
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

//export fauna_conversations_uniffiFreeGorutine
func fauna_conversations_uniffiFreeGorutine(data C.uint64_t) {
	handle := cgo.Handle(uintptr(data))
	defer handle.Delete()

	guard := handle.Value().(chan struct{})
	guard <- struct{}{}
}

// FFI-exported twin of [`Rail::as_str`] — the counterpart to [`rail_parse`]
// for native apps (windows / macos / ios / android) that currently
// hand-duplicate this name mapping rather than depending on the crate
// directly. linux and tui call [`Rail::as_str`] / [`Rail::parse`] directly.
func RailAsStr(rail Rail) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_rail_as_str(FfiConverterRailINSTANCE.Lower(rail), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`Rail::glyph`] for native apps (windows / macos /
// ios) that need the icon concept for an ad-hoc [`Rail`] with no precomputed
// snapshot `glyph` — e.g. the recipient-picker suggestion, which derives a
// rail from a typed address rather than a thread. Thread rows read
// `ThreadSummary` / `ThreadDetail::glyph` directly; this is the same mapping
// for the off-snapshot case, keeping the concept in Rust (D5). linux calls
// `Rail::glyph` directly as a crate dep.
func RailGlyph(rail Rail) fauna_core.SourceGlyph {
	return fauna_core.FfiConverterSourceGlyphINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_rail_glyph(FfiConverterRailINSTANCE.Lower(rail), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`Rail::parse`]. See [`rail_as_str`].
func RailParse(s string) Rail {
	return FfiConverterRailINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_rail_parse(FfiConverterStringINSTANCE.Lower(s), _uniffiStatus),
		}
	}))
}

// Format-only synchronous parse of a user-typed string into a
// [`TypedAddress`]. Mirrors the previous C# `ParseTypedAddress` helper —
// no rail probing, just the prefix/shape recognizers the recipient
// picker needs to drive its `resolve_state` while the user types.
//
// Cannot produce [`TypedAddress::Fauna`]: that variant requires an
// `ActorId` (32-byte public key) which only a real fauna_mls backend
// probe can supply. A typed Fauna handle parses here as Email until
// the backend resolves it asynchronously.
//
// Cannot produce [`TypedAddress::Bridged`] either: which bridge a far-network
// spelling belongs to is the nest's answer, matched against each bridge's
// declared grammar (`ui/conversations.md` § Where logic lives → *The
// `Bridged` adapter*, ruling 2 (d)), so no app parses one. A `did:`, a
// two-`@` Fediverse handle or an `npub1…` — the shapes the retired Bluesky,
// ActivityPub and Nostr rails claimed here — is therefore not recognised,
// and neither is read as an email address.
//
// FFI-exported so the native apps (windows/macos/ios/android) consume this
// one recognizer for the `dm-reply-recipient-add` field + recipient picker
// instead of each maintaining a per-app duplicate (priority #2/#4); linux
// calls it directly as a Rust crate dep.
func TryParseTypedAddress(raw string) *TypedAddress {
	return FfiConverterOptionalTypedAddressINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_try_parse_typed_address(FfiConverterStringINSTANCE.Lower(raw), _uniffiStatus),
		}
	}))
}

// [`ResolveState`] → its [`ResolveStatusView`] (token + status label). See the
// view's doc for the contract; native apps pass their typed snapshot state.
func RecipientResolveStatus(state ResolveState) ResolveStatusView {
	return FfiConverterResolveStatusViewINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_recipient_resolve_status(FfiConverterResolveStateINSTANCE.Lower(state), _uniffiStatus),
		}
	}))
}

// FFI face of [`MORE_GRID_EMOJIS`] — the [`quickset_emojis`] shape, for windows over
// UniFFI and web through the wasm twin (`moreGridEmojis` in `libs/fauna-wasm`).
func MoreGridEmojis() []string {
	return FfiConverterSequenceStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_more_grid_emojis(_uniffiStatus),
		}
	}))
}

// FFI face of [`QUICKSET_EMOJIS`] — the quick-set, in order, for the apps that
// reach shared Rust across a binding rather than as a crate dep (apple, android,
// windows; web via the wasm twin).
//
// A `const` array of `&str` has no UniFFI representation, so the face is a
// function returning owned `String`s — the same shape [`rail_glyph`](crate::rail_glyph)
// takes for the other cross-binding constant-ish projection. Ordering is the
// contract, not just the membership: `dm-reaction-option` is an indexed element,
// so an e2e that taps index 0 is asserting "👍" on every app.
func QuicksetEmojis() []string {
	return FfiConverterSequenceStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_quickset_emojis(_uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomClass::attr_token`] — the `class` attribute.
func RoomClassAttrToken(class RoomClass) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_class_attr_token(FfiConverterRoomClassINSTANCE.Lower(class), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomClass::label`] — what `thread-room-class` and
// `recipient-picker-class` state.
func RoomClassLabel(class RoomClass) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_class_label(FfiConverterRoomClassINSTANCE.Lower(class), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`HistoryPolicy::EDITOR_CHOICES`] — what
// `room-history-policy-select` offers, in its order.
func RoomHistoryPolicyEditorChoices() []HistoryPolicy {
	return FfiConverterSequenceHistoryPolicyINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_history_policy_editor_choices(_uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`history_policy_label`].
func RoomHistoryPolicyLabel(policy HistoryPolicy) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_history_policy_label(FfiConverterHistoryPolicyINSTANCE.Lower(policy), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`HistoryPolicy::token`].
func RoomHistoryPolicyToken(policy HistoryPolicy) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_history_policy_token(FfiConverterHistoryPolicyINSTANCE.Lower(policy), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomInvitationSnapshot::text`] — the
// `room-invitation[i]` row's sentence.
func RoomInvitationText(invitation RoomInvitationSnapshot) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_invitation_text(FfiConverterRoomInvitationSnapshotINSTANCE.Lower(invitation), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`JoinRule::EDITOR_CHOICES`] — what
// `room-join-rule-select` offers, in its order.
func RoomJoinRuleEditorChoices() []JoinRule {
	return FfiConverterSequenceJoinRuleINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_join_rule_editor_choices(_uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`join_rule_label`].
func RoomJoinRuleLabel(rule JoinRule) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_join_rule_label(FfiConverterJoinRuleINSTANCE.Lower(rule), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`JoinRule::token`].
func RoomJoinRuleToken(rule JoinRule) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_join_rule_token(FfiConverterJoinRuleINSTANCE.Lower(rule), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`member_chip_text`].
func RoomMemberChipText(display string, role *RoomRole) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_member_chip_text(FfiConverterStringINSTANCE.Lower(display), FfiConverterOptionalRoomRoleINSTANCE.Lower(role), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomNotice::attr_token`] — the `state` attribute.
func RoomNoticeAttrToken(notice RoomNotice) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_notice_attr_token(FfiConverterRoomNoticeINSTANCE.Lower(notice), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomNotice::for_facts`] — which notice, if any,
// `thread-room-notice` states for a room's `awaiting_key` and
// `moderation_unverified`.
func RoomNoticeFor(awaitingKey bool, moderationUnverified bool) *RoomNotice {
	return FfiConverterOptionalRoomNoticeINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_notice_for(FfiConverterBoolINSTANCE.Lower(awaitingKey), FfiConverterBoolINSTANCE.Lower(moderationUnverified), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomNotice::label`].
func RoomNoticeLabel(notice RoomNotice) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_notice_label(FfiConverterRoomNoticeINSTANCE.Lower(notice), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomPendingInviteSnapshot::text`] — the
// `room-pending-invite[i]` row's sentence.
func RoomPendingInviteText(invite RoomPendingInviteSnapshot) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_pending_invite_text(FfiConverterRoomPendingInviteSnapshotINSTANCE.Lower(invite), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`prospective_room_class`] — the new-thread picker's
// `recipient-picker-class`, from its committed chips and the home-nest choice
// (`RecipientPickerState::include_home_nest`).
func RoomProspectiveClass(chips []TypedAddress, includeHomeNest bool) *RoomClass {
	return FfiConverterOptionalRoomClassINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_prospective_class(FfiConverterSequenceTypedAddressINSTANCE.Lower(chips), FfiConverterBoolINSTANCE.Lower(includeHomeNest), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomRole::attr_token`] — the member chip's `role`
// attribute.
func RoomRoleAttrToken(role RoomRole) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_role_attr_token(FfiConverterRoomRoleINSTANCE.Lower(role), _uniffiStatus),
		}
	}))
}

// Whether a community room may name a labeler of this artifact kind
// (`LabelerCatalogEntry::artifact_kind`, already normalized by the catalog
// machine) — the filter every editor applies to the catalog before painting
// a `room-labeler-toggle` row, over the list the home nest admits by
// (`fauna_mls::room_policy::ROOM_LABELER_KINDS`).
func RoomMayNameLabelerKind(kind string) bool {
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_conversations_fn_func_room_may_name_labeler_kind(FfiConverterStringINSTANCE.Lower(kind), _uniffiStatus)
	}))
}

// FFI-exported twin of [`RoomSettingsDraft::admin_at`] — the `checked` a
// painter puts on `room-admin-toggle[i]`.
func RoomSettingsAdminAt(draft RoomSettingsDraft, participants []TypedAddress, index uint32) bool {
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_conversations_fn_func_room_settings_admin_at(FfiConverterRoomSettingsDraftINSTANCE.Lower(draft), FfiConverterSequenceTypedAddressINSTANCE.Lower(participants), FfiConverterUint32INSTANCE.Lower(index), _uniffiStatus)
	}))
}

// FFI-exported twin of [`RoomSettingsDraft::edits`].
func RoomSettingsEdits(draft RoomSettingsDraft, participants []TypedAddress) []RoomSettingsEdit {
	return FfiConverterSequenceRoomSettingsEditINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_settings_edits(FfiConverterRoomSettingsDraftINSTANCE.Lower(draft), FfiConverterSequenceTypedAddressINSTANCE.Lower(participants), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomSettingsDraft::is_eligible`].
func RoomSettingsIsEligible(draft RoomSettingsDraft, participants []TypedAddress, index uint32) bool {
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_conversations_fn_func_room_settings_is_eligible(FfiConverterRoomSettingsDraftINSTANCE.Lower(draft), FfiConverterSequenceTypedAddressINSTANCE.Lower(participants), FfiConverterUint32INSTANCE.Lower(index), _uniffiStatus)
	}))
}

// FFI-exported twin of [`RoomSettingsDraft::is_owner_at`].
func RoomSettingsIsOwnerAt(draft RoomSettingsDraft, participants []TypedAddress, index uint32) bool {
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_conversations_fn_func_room_settings_is_owner_at(FfiConverterRoomSettingsDraftINSTANCE.Lower(draft), FfiConverterSequenceTypedAddressINSTANCE.Lower(participants), FfiConverterUint32INSTANCE.Lower(index), _uniffiStatus)
	}))
}

// FFI-exported twin of [`RoomSettingsDraft::labeler_staged`].
func RoomSettingsLabelerStaged(draft RoomSettingsDraft, labeler string) bool {
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_conversations_fn_func_room_settings_labeler_staged(FfiConverterRoomSettingsDraftINSTANCE.Lower(draft), FfiConverterStringINSTANCE.Lower(labeler), _uniffiStatus)
	}))
}

// FFI-exported twin of [`RoomSettingsDraft::labeler_toggle_live`].
func RoomSettingsLabelerToggleLive(draft RoomSettingsDraft, labeler string) bool {
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_conversations_fn_func_room_settings_labeler_toggle_live(FfiConverterRoomSettingsDraftINSTANCE.Lower(draft), FfiConverterStringINSTANCE.Lower(labeler), _uniffiStatus)
	}))
}

// FFI-exported twin of [`RoomSettingsDraft::seed`].
func RoomSettingsSeed(detail ThreadDetail) *RoomSettingsDraft {
	return FfiConverterOptionalRoomSettingsDraftINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_settings_seed(FfiConverterThreadDetailINSTANCE.Lower(detail), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomSettingsDraft::set_history_policy_token`].
func RoomSettingsSetHistoryPolicy(draft RoomSettingsDraft, token string) RoomSettingsDraft {
	return FfiConverterRoomSettingsDraftINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_settings_set_history_policy(FfiConverterRoomSettingsDraftINSTANCE.Lower(draft), FfiConverterStringINSTANCE.Lower(token), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomSettingsDraft::set_join_rule_token`].
func RoomSettingsSetJoinRule(draft RoomSettingsDraft, token string) RoomSettingsDraft {
	return FfiConverterRoomSettingsDraftINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_settings_set_join_rule(FfiConverterRoomSettingsDraftINSTANCE.Lower(draft), FfiConverterStringINSTANCE.Lower(token), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomSettingsDraft::toggle_admin`].
//
// Takes the **live** participant list beside the paint index, like
// [`room_settings_edits`] already does: the index names a row on screen, and
// only the live list says who that is (`RoomSettingsDraft`'s own doc carries
// the argument).
func RoomSettingsToggleAdmin(draft RoomSettingsDraft, participants []TypedAddress, index uint32) RoomSettingsDraft {
	return FfiConverterRoomSettingsDraftINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_settings_toggle_admin(FfiConverterRoomSettingsDraftINSTANCE.Lower(draft), FfiConverterSequenceTypedAddressINSTANCE.Lower(participants), FfiConverterUint32INSTANCE.Lower(index), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomSettingsDraft::toggle_labeler`].
func RoomSettingsToggleLabeler(draft RoomSettingsDraft, labeler string) RoomSettingsDraft {
	return FfiConverterRoomSettingsDraftINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_settings_toggle_labeler(FfiConverterRoomSettingsDraftINSTANCE.Lower(draft), FfiConverterStringINSTANCE.Lower(labeler), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomSettingsDraft::toggle_nest_read`].
func RoomSettingsToggleNestRead(draft RoomSettingsDraft) RoomSettingsDraft {
	return FfiConverterRoomSettingsDraftINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_settings_toggle_nest_read(FfiConverterRoomSettingsDraftINSTANCE.Lower(draft), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomSettingsDraft::toggle_transfer`].
func RoomSettingsToggleTransfer(draft RoomSettingsDraft, participants []TypedAddress, index uint32) RoomSettingsDraft {
	return FfiConverterRoomSettingsDraftINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_room_settings_toggle_transfer(FfiConverterRoomSettingsDraftINSTANCE.Lower(draft), FfiConverterSequenceTypedAddressINSTANCE.Lower(participants), FfiConverterUint32INSTANCE.Lower(index), _uniffiStatus),
		}
	}))
}

// FFI-exported twin of [`RoomSettingsDraft::transfer_staged_at`].
func RoomSettingsTransferStagedAt(draft RoomSettingsDraft, participants []TypedAddress, index uint32) bool {
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_conversations_fn_func_room_settings_transfer_staged_at(FfiConverterRoomSettingsDraftINSTANCE.Lower(draft), FfiConverterSequenceTypedAddressINSTANCE.Lower(participants), FfiConverterUint32INSTANCE.Lower(index), _uniffiStatus)
	}))
}

// The `conversation-sort` button's canonical cycle: latest activity → oldest
// first → unread → back to latest. One tap advances one step; three taps from
// any order return home, so every order stays reachable on every app.
//
// This owns the *cycle*; [`crate::manager::ConversationsManager::set_sort`]
// owns the resulting reorder (`sort_summaries`). Clients pass the order the
// snapshot handed them and feed the result straight back to `set_sort` — they
// never enumerate the orders themselves. That is what keeps the arity uniform:
// each app used to hand-roll it, and they disagreed — android cycled all
// three while web and apple reached only two, leaving [`SortOrder::Unread`]
// (fully implemented in `sort_summaries`) unreachable for those users.
//
// The 3-way cycle is user-ratified (2026-07-16); see
// `docs/goal/ui/conversations.md` § Where logic lives + the `conversation-sort`
// element-table row.
func NextSortOrder(current SortOrder) SortOrder {
	return FfiConverterSortOrderINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_conversations_fn_func_next_sort_order(FfiConverterSortOrderINSTANCE.Lower(current), _uniffiStatus),
		}
	}))
}
