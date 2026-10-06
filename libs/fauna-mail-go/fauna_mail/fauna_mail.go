package fauna_mail

// #include <fauna_mail.h>
import "C"

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"
	"io"
	"math"
	"reflect"
	"runtime/cgo"
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
		C.ffi_fauna_mail_rustbuffer_free(cb.inner, status)
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
		return C.ffi_fauna_mail_rustbuffer_from_bytes(foreign, status)
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
		return C.ffi_fauna_mail_uniffi_contract_version()
	})
	if bindingsContractVersion != int(scaffoldingContractVersion) {
		// If this happens try cleaning and rebuilding your project
		panic("fauna_mail: UniFFI contract version mismatch")
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_verify_inbound()
		})
		if checksum != 17739 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_verify_inbound: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_join_sealed_mail_body()
		})
		if checksum != 41810 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_join_sealed_mail_body: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_mail_body_needs_reference()
		})
		if checksum != 29930 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_mail_body_needs_reference: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_split_sealed_mail_body()
		})
		if checksum != 36441 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_split_sealed_mail_body: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_fetch_binary_section()
		})
		if checksum != 27570 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_fetch_binary_section: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_fetch_binary_size()
		})
		if checksum != 58055 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_fetch_binary_size: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_fetch_body_section()
		})
		if checksum != 15108 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_fetch_body_section: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_derive_body_structure()
		})
		if checksum != 8635 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_derive_body_structure: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_mail_dedup_keys()
		})
		if checksum != 31960 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_mail_dedup_keys: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_derive_envelope()
		})
		if checksum != 37665 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_derive_envelope: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_from_mailboxes()
		})
		if checksum != 54671 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_from_mailboxes: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_sender_domain_with_envelope_fallback()
		})
		if checksum != 20556 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_sender_domain_with_envelope_fallback: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_auto_reply_decision()
		})
		if checksum != 8011 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_auto_reply_decision: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_evaluate()
		})
		if checksum != 26041 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_evaluate: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_from_field_count()
		})
		if checksum != 55601 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_from_field_count: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_build_attendee_reply_from_ics()
		})
		if checksum != 53055 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_build_attendee_reply_from_ics: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_build_event_imip_from_ics()
		})
		if checksum != 60606 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_build_event_imip_from_ics: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_expand_recurrence()
		})
		if checksum != 47925 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_expand_recurrence: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_generate_ical()
		})
		if checksum != 28396 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_generate_ical: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_generate_itip()
		})
		if checksum != 56363 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_generate_itip: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_invite_from_mail()
		})
		if checksum != 29611 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_invite_from_mail: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_parse_ical_organizer_from_ics()
		})
		if checksum != 21002 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_parse_ical_organizer_from_ics: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_parse_icalendar()
		})
		if checksum != 42085 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_parse_icalendar: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_lookup_kind()
		})
		if checksum != 61201 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_lookup_kind: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_new_fauna_msgid_local()
		})
		if checksum != 30129 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_new_fauna_msgid_local: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_compose_auto_reply()
		})
		if checksum != 32852 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_compose_auto_reply: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_parse_rfc5322()
		})
		if checksum != 62289 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_parse_rfc5322: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_build_received_header()
		})
		if checksum != 33914 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_build_received_header: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_report_hash()
		})
		if checksum != 65304 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_report_hash: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_clamd_parse_reply()
		})
		if checksum != 35335 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_clamd_parse_reply: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_decide_scan_action()
		})
		if checksum != 22646 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_decide_scan_action: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_rspamd_parse_reply()
		})
		if checksum != 42428 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_rspamd_parse_reply: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_build_scheduling_delivery()
		})
		if checksum != 17740 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_build_scheduling_delivery: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_build_authenticated_sender_stamp()
		})
		if checksum != 36929 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_build_authenticated_sender_stamp: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_apply_unlisted_recipient_penalty_milli()
		})
		if checksum != 53898 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_apply_unlisted_recipient_penalty_milli: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_combined_spam_score_milli()
		})
		if checksum != 51836 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_combined_spam_score_milli: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_decide_spam_disposition()
		})
		if checksum != 46321 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_decide_spam_disposition: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_apply_spam_training()
		})
		if checksum != 31935 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_apply_spam_training: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_default_bayesian_knobs()
		})
		if checksum != 46416 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_default_bayesian_knobs: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_fold_spam_model_baseline()
		})
		if checksum != 50289 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_fold_spam_model_baseline: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_weighted_bayesian_milli_for_model()
		})
		if checksum != 33271 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_weighted_bayesian_milli_for_model: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_open_staged_body()
		})
		if checksum != 50242 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_open_staged_body: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_seal_staged_body()
		})
		if checksum != 9716 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_seal_staged_body: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_tokenize()
		})
		if checksum != 37467 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_tokenize: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_effective_max_raw_message_bytes()
		})
		if checksum != 30991 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_effective_max_raw_message_bytes: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_inline_mail_request_budget_bytes()
		})
		if checksum != 1198 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_inline_mail_request_budget_bytes: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_max_inline_raw_message_bytes()
		})
		if checksum != 56318 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_max_inline_raw_message_bytes: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_fauna_mail_checksum_func_max_message_bytes_ceiling()
		})
		if checksum != 54764 {
			// If this happens try cleaning and rebuilding your project
			panic("fauna_mail: uniffi_fauna_mail_checksum_func_max_message_bytes_ceiling: UniFFI API checksum mismatch")
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

// The inputs for one composed auto-reply. All addresses/dates/ids are formatted
// by the caller (mirrors `dsn::DsnReport`, where the caller formats dates).
type AutoReplyMessage struct {
	// `From:` — the recipient's address (the vacationing user). The reply is
	// DKIM-signed under this address's domain by the caller.
	FromAddr string
	// `To:` — the envelope sender we are replying to.
	ToAddr string
	// `Subject:` — user-authored (sanitized to a single header line here).
	Subject string
	// The text/plain body — user-authored.
	Body string
	// The triggering message's `Message-ID` (with angle brackets) for
	// `In-Reply-To`/`References`; empty if it had none (those headers are then
	// omitted).
	InReplyTo string
	// A freshly-generated `Message-ID` for this reply (with angle brackets).
	MessageId string
	// RFC 5322 date-time for the `Date:` header.
	Date string
}

func (r *AutoReplyMessage) Destroy() {
	FfiDestroyerString{}.Destroy(r.FromAddr)
	FfiDestroyerString{}.Destroy(r.ToAddr)
	FfiDestroyerString{}.Destroy(r.Subject)
	FfiDestroyerString{}.Destroy(r.Body)
	FfiDestroyerString{}.Destroy(r.InReplyTo)
	FfiDestroyerString{}.Destroy(r.MessageId)
	FfiDestroyerString{}.Destroy(r.Date)
}

type FfiConverterAutoReplyMessage struct{}

var FfiConverterAutoReplyMessageINSTANCE = FfiConverterAutoReplyMessage{}

func (c FfiConverterAutoReplyMessage) Lift(rb RustBufferI) AutoReplyMessage {
	return LiftFromRustBuffer[AutoReplyMessage](c, rb)
}

func (c FfiConverterAutoReplyMessage) Read(reader io.Reader) AutoReplyMessage {
	return AutoReplyMessage{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterAutoReplyMessage) Lower(value AutoReplyMessage) C.RustBuffer {
	return LowerIntoRustBuffer[AutoReplyMessage](c, value)
}

func (c FfiConverterAutoReplyMessage) LowerExternal(value AutoReplyMessage) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AutoReplyMessage](c, value))
}

func (c FfiConverterAutoReplyMessage) Write(writer io.Writer, value AutoReplyMessage) {
	FfiConverterStringINSTANCE.Write(writer, value.FromAddr)
	FfiConverterStringINSTANCE.Write(writer, value.ToAddr)
	FfiConverterStringINSTANCE.Write(writer, value.Subject)
	FfiConverterStringINSTANCE.Write(writer, value.Body)
	FfiConverterStringINSTANCE.Write(writer, value.InReplyTo)
	FfiConverterStringINSTANCE.Write(writer, value.MessageId)
	FfiConverterStringINSTANCE.Write(writer, value.Date)
}

type FfiDestroyerAutoReplyMessage struct{}

func (_ FfiDestroyerAutoReplyMessage) Destroy(value AutoReplyMessage) {
	value.Destroy()
}

// Confidence-ramp + weight knobs for `weighted_bayesian_milli`. The defaults
// are the § Combined-score formula values (`bayesian_weight = 0.7`,
// `bayesian_min_samples = 50`, `bayesian_full_confidence_samples = 200`); the
// Tier-2 `mail.spam.bayesian_*` admin overrides are projected on the
// `SpamPolicyThresholds` wire sub-struct and seeded into the off-nest scorer
// at the search-equivalent position (`mail-policy-config.md` § Spam). The
// weight is carried as milli (`700`) to avoid a float knob.
type BayesianKnobs struct {
	BayesianWeightMilli   uint32
	MinSamples            uint32
	FullConfidenceSamples uint32
}

func (r *BayesianKnobs) Destroy() {
	FfiDestroyerUint32{}.Destroy(r.BayesianWeightMilli)
	FfiDestroyerUint32{}.Destroy(r.MinSamples)
	FfiDestroyerUint32{}.Destroy(r.FullConfidenceSamples)
}

type FfiConverterBayesianKnobs struct{}

var FfiConverterBayesianKnobsINSTANCE = FfiConverterBayesianKnobs{}

func (c FfiConverterBayesianKnobs) Lift(rb RustBufferI) BayesianKnobs {
	return LiftFromRustBuffer[BayesianKnobs](c, rb)
}

func (c FfiConverterBayesianKnobs) Read(reader io.Reader) BayesianKnobs {
	return BayesianKnobs{
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
	}
}

func (c FfiConverterBayesianKnobs) Lower(value BayesianKnobs) C.RustBuffer {
	return LowerIntoRustBuffer[BayesianKnobs](c, value)
}

func (c FfiConverterBayesianKnobs) LowerExternal(value BayesianKnobs) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[BayesianKnobs](c, value))
}

func (c FfiConverterBayesianKnobs) Write(writer io.Writer, value BayesianKnobs) {
	FfiConverterUint32INSTANCE.Write(writer, value.BayesianWeightMilli)
	FfiConverterUint32INSTANCE.Write(writer, value.MinSamples)
	FfiConverterUint32INSTANCE.Write(writer, value.FullConfidenceSamples)
}

type FfiDestroyerBayesianKnobs struct{}

func (_ FfiDestroyerBayesianKnobs) Destroy(value BayesianKnobs) {
	value.Destroy()
}

// A parsed IMAP `BINARY[<section-binary>]` request (RFC 3516 / RFC 9051
// §6.4.5), mirroring the go-imap fork's `imap.FetchItemBinarySection`. A
// binary section addresses a numbered MIME part (or the whole message when
// `part` is empty) and returns its `Content-Transfer-Encoding`-decoded
// contents — there is no `HEADER` / `TEXT` / `MIME` sub-specifier (those are
// `BODY[…]` forms).
type BinarySectionSpec struct {
	// Numbered MIME part path (e.g. `[1, 2]` for part `1.2`). Empty = the
	// whole message (header + decoded body, like `BODY[]`).
	Part []uint32
	// `<offset.size>` partial substring, applied to the decoded section
	// last. `None` = the whole section. (`BINARY.SIZE` carries no partial.)
	Partial *BodySectionPartial
}

func (r *BinarySectionSpec) Destroy() {
	FfiDestroyerSequenceUint32{}.Destroy(r.Part)
	FfiDestroyerOptionalBodySectionPartial{}.Destroy(r.Partial)
}

type FfiConverterBinarySectionSpec struct{}

var FfiConverterBinarySectionSpecINSTANCE = FfiConverterBinarySectionSpec{}

func (c FfiConverterBinarySectionSpec) Lift(rb RustBufferI) BinarySectionSpec {
	return LiftFromRustBuffer[BinarySectionSpec](c, rb)
}

func (c FfiConverterBinarySectionSpec) Read(reader io.Reader) BinarySectionSpec {
	return BinarySectionSpec{
		FfiConverterSequenceUint32INSTANCE.Read(reader),
		FfiConverterOptionalBodySectionPartialINSTANCE.Read(reader),
	}
}

func (c FfiConverterBinarySectionSpec) Lower(value BinarySectionSpec) C.RustBuffer {
	return LowerIntoRustBuffer[BinarySectionSpec](c, value)
}

func (c FfiConverterBinarySectionSpec) LowerExternal(value BinarySectionSpec) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[BinarySectionSpec](c, value))
}

func (c FfiConverterBinarySectionSpec) Write(writer io.Writer, value BinarySectionSpec) {
	FfiConverterSequenceUint32INSTANCE.Write(writer, value.Part)
	FfiConverterOptionalBodySectionPartialINSTANCE.Write(writer, value.Partial)
}

type FfiDestroyerBinarySectionSpec struct{}

func (_ FfiDestroyerBinarySectionSpec) Destroy(value BinarySectionSpec) {
	value.Destroy()
}

// The RFC 9051 §6.4.5 `<partial>` origin-octet / length pair.
type BodySectionPartial struct {
	// Origin octet, 0-based.
	Offset uint64
	// Maximum number of octets to return starting at `offset`.
	Size uint64
}

func (r *BodySectionPartial) Destroy() {
	FfiDestroyerUint64{}.Destroy(r.Offset)
	FfiDestroyerUint64{}.Destroy(r.Size)
}

type FfiConverterBodySectionPartial struct{}

var FfiConverterBodySectionPartialINSTANCE = FfiConverterBodySectionPartial{}

func (c FfiConverterBodySectionPartial) Lift(rb RustBufferI) BodySectionPartial {
	return LiftFromRustBuffer[BodySectionPartial](c, rb)
}

func (c FfiConverterBodySectionPartial) Read(reader io.Reader) BodySectionPartial {
	return BodySectionPartial{
		FfiConverterUint64INSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
	}
}

func (c FfiConverterBodySectionPartial) Lower(value BodySectionPartial) C.RustBuffer {
	return LowerIntoRustBuffer[BodySectionPartial](c, value)
}

func (c FfiConverterBodySectionPartial) LowerExternal(value BodySectionPartial) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[BodySectionPartial](c, value))
}

func (c FfiConverterBodySectionPartial) Write(writer io.Writer, value BodySectionPartial) {
	FfiConverterUint64INSTANCE.Write(writer, value.Offset)
	FfiConverterUint64INSTANCE.Write(writer, value.Size)
}

type FfiDestroyerBodySectionPartial struct{}

func (_ FfiDestroyerBodySectionPartial) Destroy(value BodySectionPartial) {
	value.Destroy()
}

// A parsed IMAP `BODY[<section>]` request, mirroring the relevant fields of
// the go-imap fork's `imap.FetchItemBodySection`. The bridge fills this from
// the value its IMAP command parser already produced.
type BodySectionSpec struct {
	// The part specifier, upper-cased: `""` (whole / none / a numbered part's
	// contents), `"HEADER"`, `"TEXT"`, or `"MIME"`. `"MIME"` is valid only with
	// a non-empty [`Self::part`] (a top-level `MIME` is rejected, as it only
	// has meaning for a numbered part).
	Specifier string
	// Numbered MIME part path (e.g. `[1, 2]` for part `1.2`). Empty = the
	// top-level section. A non-empty path addresses one MIME part, recursing
	// into `multipart/<subtype>` and encapsulated `message/rfc822` per RFC 9051
	// §6.4.5.
	Part []uint32
	// `HEADER.FIELDS (f1 f2 …)` field names. Non-empty ⇒ keep only the named
	// headers (case-insensitive match), in message order.
	HeaderFields []string
	// `HEADER.FIELDS.NOT (f1 f2 …)` field names. Non-empty ⇒ keep every
	// header except the named ones, in message order.
	HeaderFieldsNot []string
	// `<offset.size>` partial substring, applied to the extracted section
	// last. `None` = the whole section.
	Partial *BodySectionPartial
}

func (r *BodySectionSpec) Destroy() {
	FfiDestroyerString{}.Destroy(r.Specifier)
	FfiDestroyerSequenceUint32{}.Destroy(r.Part)
	FfiDestroyerSequenceString{}.Destroy(r.HeaderFields)
	FfiDestroyerSequenceString{}.Destroy(r.HeaderFieldsNot)
	FfiDestroyerOptionalBodySectionPartial{}.Destroy(r.Partial)
}

type FfiConverterBodySectionSpec struct{}

var FfiConverterBodySectionSpecINSTANCE = FfiConverterBodySectionSpec{}

func (c FfiConverterBodySectionSpec) Lift(rb RustBufferI) BodySectionSpec {
	return LiftFromRustBuffer[BodySectionSpec](c, rb)
}

func (c FfiConverterBodySectionSpec) Read(reader io.Reader) BodySectionSpec {
	return BodySectionSpec{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterSequenceUint32INSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterOptionalBodySectionPartialINSTANCE.Read(reader),
	}
}

func (c FfiConverterBodySectionSpec) Lower(value BodySectionSpec) C.RustBuffer {
	return LowerIntoRustBuffer[BodySectionSpec](c, value)
}

func (c FfiConverterBodySectionSpec) LowerExternal(value BodySectionSpec) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[BodySectionSpec](c, value))
}

func (c FfiConverterBodySectionSpec) Write(writer io.Writer, value BodySectionSpec) {
	FfiConverterStringINSTANCE.Write(writer, value.Specifier)
	FfiConverterSequenceUint32INSTANCE.Write(writer, value.Part)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.HeaderFields)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.HeaderFieldsNot)
	FfiConverterOptionalBodySectionPartialINSTANCE.Write(writer, value.Partial)
}

type FfiDestroyerBodySectionSpec struct{}

func (_ FfiDestroyerBodySectionSpec) Destroy(value BodySectionSpec) {
	value.Destroy()
}

// One MIME part of a derived BODYSTRUCTURE tree.
//
// Multipart parts have `type_ == "MULTIPART"`, `parts` non-empty, and
// `encoding`/`size_octets`/`lines` unused. Leaf parts have `parts` empty
// and the leaf-only fields populated.
type BodyStructure struct {
	// Top-level MIME type, upper-cased (`"TEXT"`, `"MULTIPART"`,
	// `"APPLICATION"`, `"MESSAGE"`, …).
	Type string
	// MIME subtype, upper-cased (`"PLAIN"`, `"HTML"`, `"ALTERNATIVE"`,
	// `"OCTET-STREAM"`, …). Empty string when absent.
	Subtype string
	// Content-Type parameters (e.g. `CHARSET=utf-8`, `BOUNDARY=...`,
	// `NAME=...`). Names upper-cased, values verbatim.
	Parameters []MimeParam
	// Content-ID header value (with surrounding angle brackets if
	// present in the source).
	Id *string
	// Content-Description header value, RFC 2047 decoded by mail-parser.
	Description *string
	// Content-Transfer-Encoding, upper-cased. Unused for multipart.
	Encoding *string
	// Octet count of the part body. For text parts: the decoded body
	// length. For binary parts: the decoded body length. For multipart:
	// `0` (multipart bodies don't carry an aggregate octet count in the
	// derived shape — the IMAP layer sums children if asked).
	SizeOctets uint32
	// Line count for `text/…` parts (LF count in the decoded body).
	// `None` for non-text leaves and for multipart.
	Lines *uint32
	// Content-Disposition main value, upper-cased (`"ATTACHMENT"`,
	// `"INLINE"`). `None` when no Content-Disposition header.
	Disposition *string
	// Content-Disposition parameters (`FILENAME=...`, `SIZE=...`).
	// Names upper-cased, values verbatim.
	DispositionParameters []MimeParam
	// Children for multipart parts (in source order). Empty for leaves.
	Parts []BodyStructure
}

func (r *BodyStructure) Destroy() {
	FfiDestroyerString{}.Destroy(r.Type)
	FfiDestroyerString{}.Destroy(r.Subtype)
	FfiDestroyerSequenceMimeParam{}.Destroy(r.Parameters)
	FfiDestroyerOptionalString{}.Destroy(r.Id)
	FfiDestroyerOptionalString{}.Destroy(r.Description)
	FfiDestroyerOptionalString{}.Destroy(r.Encoding)
	FfiDestroyerUint32{}.Destroy(r.SizeOctets)
	FfiDestroyerOptionalUint32{}.Destroy(r.Lines)
	FfiDestroyerOptionalString{}.Destroy(r.Disposition)
	FfiDestroyerSequenceMimeParam{}.Destroy(r.DispositionParameters)
	FfiDestroyerSequenceBodyStructure{}.Destroy(r.Parts)
}

type FfiConverterBodyStructure struct{}

var FfiConverterBodyStructureINSTANCE = FfiConverterBodyStructure{}

func (c FfiConverterBodyStructure) Lift(rb RustBufferI) BodyStructure {
	return LiftFromRustBuffer[BodyStructure](c, rb)
}

func (c FfiConverterBodyStructure) Read(reader io.Reader) BodyStructure {
	return BodyStructure{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterSequenceMimeParamINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterOptionalUint32INSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterSequenceMimeParamINSTANCE.Read(reader),
		FfiConverterSequenceBodyStructureINSTANCE.Read(reader),
	}
}

func (c FfiConverterBodyStructure) Lower(value BodyStructure) C.RustBuffer {
	return LowerIntoRustBuffer[BodyStructure](c, value)
}

func (c FfiConverterBodyStructure) LowerExternal(value BodyStructure) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[BodyStructure](c, value))
}

func (c FfiConverterBodyStructure) Write(writer io.Writer, value BodyStructure) {
	FfiConverterStringINSTANCE.Write(writer, value.Type)
	FfiConverterStringINSTANCE.Write(writer, value.Subtype)
	FfiConverterSequenceMimeParamINSTANCE.Write(writer, value.Parameters)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Id)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Description)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Encoding)
	FfiConverterUint32INSTANCE.Write(writer, value.SizeOctets)
	FfiConverterOptionalUint32INSTANCE.Write(writer, value.Lines)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Disposition)
	FfiConverterSequenceMimeParamINSTANCE.Write(writer, value.DispositionParameters)
	FfiConverterSequenceBodyStructureINSTANCE.Write(writer, value.Parts)
}

type FfiDestroyerBodyStructure struct{}

func (_ FfiDestroyerBodyStructure) Destroy(value BodyStructure) {
	value.Destroy()
}

type CanonicalTokenSet struct {
	// Sorted, deduplicated tokens. Reading order is a function of the input,
	// not insertion order — every platform produces the same `tokens` vec
	// for a given input.
	Tokens []string
	// Length-prefixed concatenation of `tokens`. Useful as a single input
	// to a downstream encryption step. Format: for each token,
	// `<u32 BE length><utf8 bytes>`. Stable across platforms.
	CanonicalBytes []byte
}

func (r *CanonicalTokenSet) Destroy() {
	FfiDestroyerSequenceString{}.Destroy(r.Tokens)
	FfiDestroyerBytes{}.Destroy(r.CanonicalBytes)
}

type FfiConverterCanonicalTokenSet struct{}

var FfiConverterCanonicalTokenSetINSTANCE = FfiConverterCanonicalTokenSet{}

func (c FfiConverterCanonicalTokenSet) Lift(rb RustBufferI) CanonicalTokenSet {
	return LiftFromRustBuffer[CanonicalTokenSet](c, rb)
}

func (c FfiConverterCanonicalTokenSet) Read(reader io.Reader) CanonicalTokenSet {
	return CanonicalTokenSet{
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
	}
}

func (c FfiConverterCanonicalTokenSet) Lower(value CanonicalTokenSet) C.RustBuffer {
	return LowerIntoRustBuffer[CanonicalTokenSet](c, value)
}

func (c FfiConverterCanonicalTokenSet) LowerExternal(value CanonicalTokenSet) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[CanonicalTokenSet](c, value))
}

func (c FfiConverterCanonicalTokenSet) Write(writer io.Writer, value CanonicalTokenSet) {
	FfiConverterSequenceStringINSTANCE.Write(writer, value.Tokens)
	FfiConverterBytesINSTANCE.Write(writer, value.CanonicalBytes)
}

type FfiDestroyerCanonicalTokenSet struct{}

func (_ FfiDestroyerCanonicalTokenSet) Destroy(value CanonicalTokenSet) {
	value.Destroy()
}

// Derived IMAP ENVELOPE.
//
// Field order mirrors RFC 9051 §7.5.2's parenthesised list. Address
// lists are empty (not absent) when the corresponding header is missing
// or unparseable. `sender` and `reply_to` carry the `from` fallback the
// RFC mandates so the Go bridge doesn't repeat the rule.
type Envelope struct {
	// ISO-8601 / RFC 3339 timestamp of the `Date:` header. `None` when
	// the header is absent or unparseable.
	Date *string
	// `Subject:` header, RFC 2047 decoded.
	Subject *string
	From    []EnvelopeAddress
	// Defaults to `from` when the header is absent (RFC 5322 § 3.6.2).
	Sender []EnvelopeAddress
	// Defaults to `from` when the header is absent (RFC 5322 § 3.6.2).
	ReplyTo []EnvelopeAddress
	To      []EnvelopeAddress
	Cc      []EnvelopeAddress
	Bcc     []EnvelopeAddress
	// `In-Reply-To:` raw header value with angle brackets preserved.
	InReplyTo *string
	// `Message-ID:` raw header value with angle brackets preserved.
	MessageId *string
}

func (r *Envelope) Destroy() {
	FfiDestroyerOptionalString{}.Destroy(r.Date)
	FfiDestroyerOptionalString{}.Destroy(r.Subject)
	FfiDestroyerSequenceEnvelopeAddress{}.Destroy(r.From)
	FfiDestroyerSequenceEnvelopeAddress{}.Destroy(r.Sender)
	FfiDestroyerSequenceEnvelopeAddress{}.Destroy(r.ReplyTo)
	FfiDestroyerSequenceEnvelopeAddress{}.Destroy(r.To)
	FfiDestroyerSequenceEnvelopeAddress{}.Destroy(r.Cc)
	FfiDestroyerSequenceEnvelopeAddress{}.Destroy(r.Bcc)
	FfiDestroyerOptionalString{}.Destroy(r.InReplyTo)
	FfiDestroyerOptionalString{}.Destroy(r.MessageId)
}

type FfiConverterEnvelope struct{}

var FfiConverterEnvelopeINSTANCE = FfiConverterEnvelope{}

func (c FfiConverterEnvelope) Lift(rb RustBufferI) Envelope {
	return LiftFromRustBuffer[Envelope](c, rb)
}

func (c FfiConverterEnvelope) Read(reader io.Reader) Envelope {
	return Envelope{
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterSequenceEnvelopeAddressINSTANCE.Read(reader),
		FfiConverterSequenceEnvelopeAddressINSTANCE.Read(reader),
		FfiConverterSequenceEnvelopeAddressINSTANCE.Read(reader),
		FfiConverterSequenceEnvelopeAddressINSTANCE.Read(reader),
		FfiConverterSequenceEnvelopeAddressINSTANCE.Read(reader),
		FfiConverterSequenceEnvelopeAddressINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterEnvelope) Lower(value Envelope) C.RustBuffer {
	return LowerIntoRustBuffer[Envelope](c, value)
}

func (c FfiConverterEnvelope) LowerExternal(value Envelope) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[Envelope](c, value))
}

func (c FfiConverterEnvelope) Write(writer io.Writer, value Envelope) {
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Date)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Subject)
	FfiConverterSequenceEnvelopeAddressINSTANCE.Write(writer, value.From)
	FfiConverterSequenceEnvelopeAddressINSTANCE.Write(writer, value.Sender)
	FfiConverterSequenceEnvelopeAddressINSTANCE.Write(writer, value.ReplyTo)
	FfiConverterSequenceEnvelopeAddressINSTANCE.Write(writer, value.To)
	FfiConverterSequenceEnvelopeAddressINSTANCE.Write(writer, value.Cc)
	FfiConverterSequenceEnvelopeAddressINSTANCE.Write(writer, value.Bcc)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.InReplyTo)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.MessageId)
}

type FfiDestroyerEnvelope struct{}

func (_ FfiDestroyerEnvelope) Destroy(value Envelope) {
	value.Destroy()
}

// One IMAP ENVELOPE address: the `(personal mailbox host)` triple from
// RFC 9051 §7.5.2 (we drop SMTP at-domain-list, which has been
// deprecated since RFC 821 and is always NIL in practice).
type EnvelopeAddress struct {
	// Display name (RFC 2047 decoded by mail-parser). `None` for bare
	// `addr@host` syntax.
	Personal *string
	// Local part (left of `@`).
	Mailbox string
	// Domain (right of `@`). Empty when the source address has no `@`.
	Host string
}

func (r *EnvelopeAddress) Destroy() {
	FfiDestroyerOptionalString{}.Destroy(r.Personal)
	FfiDestroyerString{}.Destroy(r.Mailbox)
	FfiDestroyerString{}.Destroy(r.Host)
}

type FfiConverterEnvelopeAddress struct{}

var FfiConverterEnvelopeAddressINSTANCE = FfiConverterEnvelopeAddress{}

func (c FfiConverterEnvelopeAddress) Lift(rb RustBufferI) EnvelopeAddress {
	return LiftFromRustBuffer[EnvelopeAddress](c, rb)
}

func (c FfiConverterEnvelopeAddress) Read(reader io.Reader) EnvelopeAddress {
	return EnvelopeAddress{
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterEnvelopeAddress) Lower(value EnvelopeAddress) C.RustBuffer {
	return LowerIntoRustBuffer[EnvelopeAddress](c, value)
}

func (c FfiConverterEnvelopeAddress) LowerExternal(value EnvelopeAddress) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[EnvelopeAddress](c, value))
}

func (c FfiConverterEnvelopeAddress) Write(writer io.Writer, value EnvelopeAddress) {
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Personal)
	FfiConverterStringINSTANCE.Write(writer, value.Mailbox)
	FfiConverterStringINSTANCE.Write(writer, value.Host)
}

type FfiDestroyerEnvelopeAddress struct{}

func (_ FfiDestroyerEnvelopeAddress) Destroy(value EnvelopeAddress) {
	value.Destroy()
}

// One expanded RRULE occurrence; epoch-seconds boundaries.
//
// `dtend` is computed from the component's original DTEND/DURATION offset
// plus the expanded `dtstart`; the component itself is the original
// (un-rotated) input for callers that need its other properties.
type ExpandedOccurrence struct {
	Component ICalComponent
	Dtstart   int64
	Dtend     int64
}

func (r *ExpandedOccurrence) Destroy() {
	FfiDestroyerICalComponent{}.Destroy(r.Component)
	FfiDestroyerInt64{}.Destroy(r.Dtstart)
	FfiDestroyerInt64{}.Destroy(r.Dtend)
}

type FfiConverterExpandedOccurrence struct{}

var FfiConverterExpandedOccurrenceINSTANCE = FfiConverterExpandedOccurrence{}

func (c FfiConverterExpandedOccurrence) Lift(rb RustBufferI) ExpandedOccurrence {
	return LiftFromRustBuffer[ExpandedOccurrence](c, rb)
}

func (c FfiConverterExpandedOccurrence) Read(reader io.Reader) ExpandedOccurrence {
	return ExpandedOccurrence{
		FfiConverterICalComponentINSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterInt64INSTANCE.Read(reader),
	}
}

func (c FfiConverterExpandedOccurrence) Lower(value ExpandedOccurrence) C.RustBuffer {
	return LowerIntoRustBuffer[ExpandedOccurrence](c, value)
}

func (c FfiConverterExpandedOccurrence) LowerExternal(value ExpandedOccurrence) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ExpandedOccurrence](c, value))
}

func (c FfiConverterExpandedOccurrence) Write(writer io.Writer, value ExpandedOccurrence) {
	FfiConverterICalComponentINSTANCE.Write(writer, value.Component)
	FfiConverterInt64INSTANCE.Write(writer, value.Dtstart)
	FfiConverterInt64INSTANCE.Write(writer, value.Dtend)
}

type FfiDestroyerExpandedOccurrence struct{}

func (_ FfiDestroyerExpandedOccurrence) Destroy(value ExpandedOccurrence) {
	value.Destroy()
}

// The plaintext-floor context the perimeter evaluates against. Everything here
// is available to the MTA pre-seal in both storage modes (LBC-2).
type FilterContext struct {
	// Envelope `MAIL FROM` address (empty for the null sender `<>`).
	From string
	// The `Subject` header value (unfolded; empty if absent).
	Subject string
	// All message headers (`HeaderExists` / `HeaderContains` match against these).
	Headers []FilterHeader
	// Combined spam score as a milli-int (`combined_spam_score_milli`).
	SpamScoreMilli int32
	// The decoded message body the `BodyContains` condition matches against —
	// the `text/plain` + `text/html` parts as the MTA holds them pre-seal,
	// concatenated and size-capped by the producer (`server.go`, 256 KiB).
	// Empty when the message has no decodable text body.
	Body string
}

func (r *FilterContext) Destroy() {
	FfiDestroyerString{}.Destroy(r.From)
	FfiDestroyerString{}.Destroy(r.Subject)
	FfiDestroyerSequenceFilterHeader{}.Destroy(r.Headers)
	FfiDestroyerInt32{}.Destroy(r.SpamScoreMilli)
	FfiDestroyerString{}.Destroy(r.Body)
}

type FfiConverterFilterContext struct{}

var FfiConverterFilterContextINSTANCE = FfiConverterFilterContext{}

func (c FfiConverterFilterContext) Lift(rb RustBufferI) FilterContext {
	return LiftFromRustBuffer[FilterContext](c, rb)
}

func (c FfiConverterFilterContext) Read(reader io.Reader) FilterContext {
	return FilterContext{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterSequenceFilterHeaderINSTANCE.Read(reader),
		FfiConverterInt32INSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterFilterContext) Lower(value FilterContext) C.RustBuffer {
	return LowerIntoRustBuffer[FilterContext](c, value)
}

func (c FfiConverterFilterContext) LowerExternal(value FilterContext) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[FilterContext](c, value))
}

func (c FfiConverterFilterContext) Write(writer io.Writer, value FilterContext) {
	FfiConverterStringINSTANCE.Write(writer, value.From)
	FfiConverterStringINSTANCE.Write(writer, value.Subject)
	FfiConverterSequenceFilterHeaderINSTANCE.Write(writer, value.Headers)
	FfiConverterInt32INSTANCE.Write(writer, value.SpamScoreMilli)
	FfiConverterStringINSTANCE.Write(writer, value.Body)
}

type FfiDestroyerFilterContext struct{}

func (_ FfiDestroyerFilterContext) Destroy(value FilterContext) {
	value.Destroy()
}

// One header as seen at the MTA perimeter (`name` is the field name without the
// colon; `value` is the unfolded field body).
type FilterHeader struct {
	Name  string
	Value string
}

func (r *FilterHeader) Destroy() {
	FfiDestroyerString{}.Destroy(r.Name)
	FfiDestroyerString{}.Destroy(r.Value)
}

type FfiConverterFilterHeader struct{}

var FfiConverterFilterHeaderINSTANCE = FfiConverterFilterHeader{}

func (c FfiConverterFilterHeader) Lift(rb RustBufferI) FilterHeader {
	return LiftFromRustBuffer[FilterHeader](c, rb)
}

func (c FfiConverterFilterHeader) Read(reader io.Reader) FilterHeader {
	return FilterHeader{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterFilterHeader) Lower(value FilterHeader) C.RustBuffer {
	return LowerIntoRustBuffer[FilterHeader](c, value)
}

func (c FfiConverterFilterHeader) LowerExternal(value FilterHeader) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[FilterHeader](c, value))
}

func (c FfiConverterFilterHeader) Write(writer io.Writer, value FilterHeader) {
	FfiConverterStringINSTANCE.Write(writer, value.Name)
	FfiConverterStringINSTANCE.Write(writer, value.Value)
}

type FfiDestroyerFilterHeader struct{}

func (_ FfiDestroyerFilterHeader) Destroy(value FilterHeader) {
	value.Destroy()
}

// One fired filter: which rule matched and the action to apply.
type FilterMatch struct {
	// The `id` of the matched filter.
	FilterId int64
	// The action to apply.
	Action FilterAction
}

func (r *FilterMatch) Destroy() {
	FfiDestroyerInt64{}.Destroy(r.FilterId)
	FfiDestroyerFilterAction{}.Destroy(r.Action)
}

type FfiConverterFilterMatch struct{}

var FfiConverterFilterMatchINSTANCE = FfiConverterFilterMatch{}

func (c FfiConverterFilterMatch) Lift(rb RustBufferI) FilterMatch {
	return LiftFromRustBuffer[FilterMatch](c, rb)
}

func (c FfiConverterFilterMatch) Read(reader io.Reader) FilterMatch {
	return FilterMatch{
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterFilterActionINSTANCE.Read(reader),
	}
}

func (c FfiConverterFilterMatch) Lower(value FilterMatch) C.RustBuffer {
	return LowerIntoRustBuffer[FilterMatch](c, value)
}

func (c FfiConverterFilterMatch) LowerExternal(value FilterMatch) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[FilterMatch](c, value))
}

func (c FfiConverterFilterMatch) Write(writer io.Writer, value FilterMatch) {
	FfiConverterInt64INSTANCE.Write(writer, value.FilterId)
	FfiConverterFilterActionINSTANCE.Write(writer, value.Action)
}

type FfiDestroyerFilterMatch struct{}

func (_ FfiDestroyerFilterMatch) Destroy(value FilterMatch) {
	value.Destroy()
}

// A single component (VEVENT, VTODO, VTIMEZONE, VALARM, ...).
type ICalComponent struct {
	Name          string
	Properties    []ICalProperty
	SubComponents []ICalComponent
}

func (r *ICalComponent) Destroy() {
	FfiDestroyerString{}.Destroy(r.Name)
	FfiDestroyerSequenceICalProperty{}.Destroy(r.Properties)
	FfiDestroyerSequenceICalComponent{}.Destroy(r.SubComponents)
}

type FfiConverterICalComponent struct{}

var FfiConverterICalComponentINSTANCE = FfiConverterICalComponent{}

func (c FfiConverterICalComponent) Lift(rb RustBufferI) ICalComponent {
	return LiftFromRustBuffer[ICalComponent](c, rb)
}

func (c FfiConverterICalComponent) Read(reader io.Reader) ICalComponent {
	return ICalComponent{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterSequenceICalPropertyINSTANCE.Read(reader),
		FfiConverterSequenceICalComponentINSTANCE.Read(reader),
	}
}

func (c FfiConverterICalComponent) Lower(value ICalComponent) C.RustBuffer {
	return LowerIntoRustBuffer[ICalComponent](c, value)
}

func (c FfiConverterICalComponent) LowerExternal(value ICalComponent) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ICalComponent](c, value))
}

func (c FfiConverterICalComponent) Write(writer io.Writer, value ICalComponent) {
	FfiConverterStringINSTANCE.Write(writer, value.Name)
	FfiConverterSequenceICalPropertyINSTANCE.Write(writer, value.Properties)
	FfiConverterSequenceICalComponentINSTANCE.Write(writer, value.SubComponents)
}

type FfiDestroyerICalComponent struct{}

func (_ FfiDestroyerICalComponent) Destroy(value ICalComponent) {
	value.Destroy()
}

// A parsed VCALENDAR document tree. The top-level [`Self::components`]
// holds VCALENDAR's children (VEVENT / VTODO / VTIMEZONE / etc.);
// [`Self::properties`] holds VCALENDAR's own properties (`VERSION`,
// `PRODID`, etc.).
type ICalDocument struct {
	Components []ICalComponent
	Properties []ICalProperty
}

func (r *ICalDocument) Destroy() {
	FfiDestroyerSequenceICalComponent{}.Destroy(r.Components)
	FfiDestroyerSequenceICalProperty{}.Destroy(r.Properties)
}

type FfiConverterICalDocument struct{}

var FfiConverterICalDocumentINSTANCE = FfiConverterICalDocument{}

func (c FfiConverterICalDocument) Lift(rb RustBufferI) ICalDocument {
	return LiftFromRustBuffer[ICalDocument](c, rb)
}

func (c FfiConverterICalDocument) Read(reader io.Reader) ICalDocument {
	return ICalDocument{
		FfiConverterSequenceICalComponentINSTANCE.Read(reader),
		FfiConverterSequenceICalPropertyINSTANCE.Read(reader),
	}
}

func (c FfiConverterICalDocument) Lower(value ICalDocument) C.RustBuffer {
	return LowerIntoRustBuffer[ICalDocument](c, value)
}

func (c FfiConverterICalDocument) LowerExternal(value ICalDocument) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ICalDocument](c, value))
}

func (c FfiConverterICalDocument) Write(writer io.Writer, value ICalDocument) {
	FfiConverterSequenceICalComponentINSTANCE.Write(writer, value.Components)
	FfiConverterSequenceICalPropertyINSTANCE.Write(writer, value.Properties)
}

type FfiDestroyerICalDocument struct{}

func (_ FfiDestroyerICalDocument) Destroy(value ICalDocument) {
	value.Destroy()
}

// One parameter on a property (UniFFI-friendly record; `(String, String)`
// tuples don't roundtrip through UniFFI as cleanly as named records).
type ICalParameter struct {
	Name  string
	Value string
}

func (r *ICalParameter) Destroy() {
	FfiDestroyerString{}.Destroy(r.Name)
	FfiDestroyerString{}.Destroy(r.Value)
}

type FfiConverterICalParameter struct{}

var FfiConverterICalParameterINSTANCE = FfiConverterICalParameter{}

func (c FfiConverterICalParameter) Lift(rb RustBufferI) ICalParameter {
	return LiftFromRustBuffer[ICalParameter](c, rb)
}

func (c FfiConverterICalParameter) Read(reader io.Reader) ICalParameter {
	return ICalParameter{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterICalParameter) Lower(value ICalParameter) C.RustBuffer {
	return LowerIntoRustBuffer[ICalParameter](c, value)
}

func (c FfiConverterICalParameter) LowerExternal(value ICalParameter) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ICalParameter](c, value))
}

func (c FfiConverterICalParameter) Write(writer io.Writer, value ICalParameter) {
	FfiConverterStringINSTANCE.Write(writer, value.Name)
	FfiConverterStringINSTANCE.Write(writer, value.Value)
}

type FfiDestroyerICalParameter struct{}

func (_ FfiDestroyerICalParameter) Destroy(value ICalParameter) {
	value.Destroy()
}

// A single property line (`NAME;PARAM=V:value`).
type ICalProperty struct {
	Name       string
	Value      string
	Parameters []ICalParameter
}

func (r *ICalProperty) Destroy() {
	FfiDestroyerString{}.Destroy(r.Name)
	FfiDestroyerString{}.Destroy(r.Value)
	FfiDestroyerSequenceICalParameter{}.Destroy(r.Parameters)
}

type FfiConverterICalProperty struct{}

var FfiConverterICalPropertyINSTANCE = FfiConverterICalProperty{}

func (c FfiConverterICalProperty) Lift(rb RustBufferI) ICalProperty {
	return LiftFromRustBuffer[ICalProperty](c, rb)
}

func (c FfiConverterICalProperty) Read(reader io.Reader) ICalProperty {
	return ICalProperty{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterSequenceICalParameterINSTANCE.Read(reader),
	}
}

func (c FfiConverterICalProperty) Lower(value ICalProperty) C.RustBuffer {
	return LowerIntoRustBuffer[ICalProperty](c, value)
}

func (c FfiConverterICalProperty) LowerExternal(value ICalProperty) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ICalProperty](c, value))
}

func (c FfiConverterICalProperty) Write(writer io.Writer, value ICalProperty) {
	FfiConverterStringINSTANCE.Write(writer, value.Name)
	FfiConverterStringINSTANCE.Write(writer, value.Value)
	FfiConverterSequenceICalParameterINSTANCE.Write(writer, value.Parameters)
}

type FfiDestroyerICalProperty struct{}

func (_ FfiDestroyerICalProperty) Destroy(value ICalProperty) {
	value.Destroy()
}

// An iMIP scheduling message ready for the Go MDA's outbound enqueue: the
// envelope `from` (→ `enqueue_outbound_mail.original_sender`), `recipients`
// (→ one queue row each), and the raw RFC 5322 message bytes (→ `raw_message`).
// UniFFI mirror of `fauna_core::ical::ImipMessage`, in the `fauna_mail`
// namespace (the cross-namespace `fauna_core` Go-binding footgun — same reason
// the writer records above live here).
type ImipDispatch struct {
	From       string
	Recipients []string
	RawRfc5322 []byte
}

func (r *ImipDispatch) Destroy() {
	FfiDestroyerString{}.Destroy(r.From)
	FfiDestroyerSequenceString{}.Destroy(r.Recipients)
	FfiDestroyerBytes{}.Destroy(r.RawRfc5322)
}

type FfiConverterImipDispatch struct{}

var FfiConverterImipDispatchINSTANCE = FfiConverterImipDispatch{}

func (c FfiConverterImipDispatch) Lift(rb RustBufferI) ImipDispatch {
	return LiftFromRustBuffer[ImipDispatch](c, rb)
}

func (c FfiConverterImipDispatch) Read(reader io.Reader) ImipDispatch {
	return ImipDispatch{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
	}
}

func (c FfiConverterImipDispatch) Lower(value ImipDispatch) C.RustBuffer {
	return LowerIntoRustBuffer[ImipDispatch](c, value)
}

func (c FfiConverterImipDispatch) LowerExternal(value ImipDispatch) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ImipDispatch](c, value))
}

func (c FfiConverterImipDispatch) Write(writer io.Writer, value ImipDispatch) {
	FfiConverterStringINSTANCE.Write(writer, value.From)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.Recipients)
	FfiConverterBytesINSTANCE.Write(writer, value.RawRfc5322)
}

type FfiDestroyerImipDispatch struct{}

func (_ FfiDestroyerImipDispatch) Destroy(value ImipDispatch) {
	value.Destroy()
}

// An invitation that arrived by email, ready to be placed on the recipient's
// calendar: its `UID` (the calendar key, hashed by the placing side exactly as a
// CalDAV PUT hashes it) and the body the calendar stores for it.
type InboundInvite struct {
	Uid string
	Ics string
}

func (r *InboundInvite) Destroy() {
	FfiDestroyerString{}.Destroy(r.Uid)
	FfiDestroyerString{}.Destroy(r.Ics)
}

type FfiConverterInboundInvite struct{}

var FfiConverterInboundInviteINSTANCE = FfiConverterInboundInvite{}

func (c FfiConverterInboundInvite) Lift(rb RustBufferI) InboundInvite {
	return LiftFromRustBuffer[InboundInvite](c, rb)
}

func (c FfiConverterInboundInvite) Read(reader io.Reader) InboundInvite {
	return InboundInvite{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterInboundInvite) Lower(value InboundInvite) C.RustBuffer {
	return LowerIntoRustBuffer[InboundInvite](c, value)
}

func (c FfiConverterInboundInvite) LowerExternal(value InboundInvite) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[InboundInvite](c, value))
}

func (c FfiConverterInboundInvite) Write(writer io.Writer, value InboundInvite) {
	FfiConverterStringINSTANCE.Write(writer, value.Uid)
	FfiConverterStringINSTANCE.Write(writer, value.Ics)
}

type FfiDestroyerInboundInvite struct{}

func (_ FfiDestroyerInboundInvite) Destroy(value InboundInvite) {
	value.Destroy()
}

type KindMetadata struct {
	ForbidReplay      bool
	DefaultDeadlineMs uint64
}

func (r *KindMetadata) Destroy() {
	FfiDestroyerBool{}.Destroy(r.ForbidReplay)
	FfiDestroyerUint64{}.Destroy(r.DefaultDeadlineMs)
}

type FfiConverterKindMetadata struct{}

var FfiConverterKindMetadataINSTANCE = FfiConverterKindMetadata{}

func (c FfiConverterKindMetadata) Lift(rb RustBufferI) KindMetadata {
	return LiftFromRustBuffer[KindMetadata](c, rb)
}

func (c FfiConverterKindMetadata) Read(reader io.Reader) KindMetadata {
	return KindMetadata{
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
	}
}

func (c FfiConverterKindMetadata) Lower(value KindMetadata) C.RustBuffer {
	return LowerIntoRustBuffer[KindMetadata](c, value)
}

func (c FfiConverterKindMetadata) LowerExternal(value KindMetadata) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[KindMetadata](c, value))
}

func (c FfiConverterKindMetadata) Write(writer io.Writer, value KindMetadata) {
	FfiConverterBoolINSTANCE.Write(writer, value.ForbidReplay)
	FfiConverterUint64INSTANCE.Write(writer, value.DefaultDeadlineMs)
}

type FfiDestroyerKindMetadata struct{}

func (_ FfiDestroyerKindMetadata) Destroy(value KindMetadata) {
	value.Destroy()
}

// One staged chunk: the bytes, and the blake3 digest that keys them in the
// content-addressed store.
type MailBodyChunk struct {
	// `blake3(bytes)` — the 32-byte store key. Sent as hex in `X-Content-Hash`
	// on upload, and carried (raw) in the RPC's body reference.
	Hash []byte
	// The chunk's sealed bytes.
	Bytes []byte
}

func (r *MailBodyChunk) Destroy() {
	FfiDestroyerBytes{}.Destroy(r.Hash)
	FfiDestroyerBytes{}.Destroy(r.Bytes)
}

type FfiConverterMailBodyChunk struct{}

var FfiConverterMailBodyChunkINSTANCE = FfiConverterMailBodyChunk{}

func (c FfiConverterMailBodyChunk) Lift(rb RustBufferI) MailBodyChunk {
	return LiftFromRustBuffer[MailBodyChunk](c, rb)
}

func (c FfiConverterMailBodyChunk) Read(reader io.Reader) MailBodyChunk {
	return MailBodyChunk{
		FfiConverterBytesINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
	}
}

func (c FfiConverterMailBodyChunk) Lower(value MailBodyChunk) C.RustBuffer {
	return LowerIntoRustBuffer[MailBodyChunk](c, value)
}

func (c FfiConverterMailBodyChunk) LowerExternal(value MailBodyChunk) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[MailBodyChunk](c, value))
}

func (c FfiConverterMailBodyChunk) Write(writer io.Writer, value MailBodyChunk) {
	FfiConverterBytesINSTANCE.Write(writer, value.Hash)
	FfiConverterBytesINSTANCE.Write(writer, value.Bytes)
}

type FfiDestroyerMailBodyChunk struct{}

func (_ FfiDestroyerMailBodyChunk) Destroy(value MailBodyChunk) {
	value.Destroy()
}

// The two keys one message is recorded under (`mailbox-migration.md`
// § Key format): the lookup key and the content-bound envelope key that
// confirms a hit (§ The envelope key confirms a Message-ID hit).
//
// Named `…Pair`, not `MailDedupKeys`: uniffi-bindgen-go maps a record and
// a function whose names differ only in case ([`mail_dedup_keys`]) to one Go
// identifier, and the binding stops compiling.
//
// A pair, never one string: a producer that could send the lookup key
// without the envelope key would re-open the hole the envelope key closes —
// a stranger-chosen Message-ID standing in for the real message at import.
type MailDedupKeyPair struct {
	// `msgid:v1:…` when the message carries a Message-ID, else the envelope
	// form — what `actor_message_dedup` is looked up by.
	DedupKey string
	// `env:v1:…`, always — the canonical-envelope hash over the five header
	// values and the body.
	EnvelopeKey string
}

func (r *MailDedupKeyPair) Destroy() {
	FfiDestroyerString{}.Destroy(r.DedupKey)
	FfiDestroyerString{}.Destroy(r.EnvelopeKey)
}

type FfiConverterMailDedupKeyPair struct{}

var FfiConverterMailDedupKeyPairINSTANCE = FfiConverterMailDedupKeyPair{}

func (c FfiConverterMailDedupKeyPair) Lift(rb RustBufferI) MailDedupKeyPair {
	return LiftFromRustBuffer[MailDedupKeyPair](c, rb)
}

func (c FfiConverterMailDedupKeyPair) Read(reader io.Reader) MailDedupKeyPair {
	return MailDedupKeyPair{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterMailDedupKeyPair) Lower(value MailDedupKeyPair) C.RustBuffer {
	return LowerIntoRustBuffer[MailDedupKeyPair](c, value)
}

func (c FfiConverterMailDedupKeyPair) LowerExternal(value MailDedupKeyPair) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[MailDedupKeyPair](c, value))
}

func (c FfiConverterMailDedupKeyPair) Write(writer io.Writer, value MailDedupKeyPair) {
	FfiConverterStringINSTANCE.Write(writer, value.DedupKey)
	FfiConverterStringINSTANCE.Write(writer, value.EnvelopeKey)
}

type FfiDestroyerMailDedupKeyPair struct{}

func (_ FfiDestroyerMailDedupKeyPair) Destroy(value MailDedupKeyPair) {
	value.Destroy()
}

// One Content-Type or Content-Disposition parameter.
//
// Keys are upper-cased per the RFC 9051 BODYSTRUCTURE convention; values
// are preserved as the parser exposes them.
type MimeParam struct {
	Name  string
	Value string
}

func (r *MimeParam) Destroy() {
	FfiDestroyerString{}.Destroy(r.Name)
	FfiDestroyerString{}.Destroy(r.Value)
}

type FfiConverterMimeParam struct{}

var FfiConverterMimeParamINSTANCE = FfiConverterMimeParam{}

func (c FfiConverterMimeParam) Lift(rb RustBufferI) MimeParam {
	return LiftFromRustBuffer[MimeParam](c, rb)
}

func (c FfiConverterMimeParam) Read(reader io.Reader) MimeParam {
	return MimeParam{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterMimeParam) Lower(value MimeParam) C.RustBuffer {
	return LowerIntoRustBuffer[MimeParam](c, value)
}

func (c FfiConverterMimeParam) LowerExternal(value MimeParam) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[MimeParam](c, value))
}

func (c FfiConverterMimeParam) Write(writer io.Writer, value MimeParam) {
	FfiConverterStringINSTANCE.Write(writer, value.Name)
	FfiConverterStringINSTANCE.Write(writer, value.Value)
}

type FfiDestroyerMimeParam struct{}

func (_ FfiDestroyerMimeParam) Destroy(value MimeParam) {
	value.Destroy()
}

type ParsedHeader struct {
	Name  string
	Value string
}

func (r *ParsedHeader) Destroy() {
	FfiDestroyerString{}.Destroy(r.Name)
	FfiDestroyerString{}.Destroy(r.Value)
}

type FfiConverterParsedHeader struct{}

var FfiConverterParsedHeaderINSTANCE = FfiConverterParsedHeader{}

func (c FfiConverterParsedHeader) Lift(rb RustBufferI) ParsedHeader {
	return LiftFromRustBuffer[ParsedHeader](c, rb)
}

func (c FfiConverterParsedHeader) Read(reader io.Reader) ParsedHeader {
	return ParsedHeader{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterParsedHeader) Lower(value ParsedHeader) C.RustBuffer {
	return LowerIntoRustBuffer[ParsedHeader](c, value)
}

func (c FfiConverterParsedHeader) LowerExternal(value ParsedHeader) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ParsedHeader](c, value))
}

func (c FfiConverterParsedHeader) Write(writer io.Writer, value ParsedHeader) {
	FfiConverterStringINSTANCE.Write(writer, value.Name)
	FfiConverterStringINSTANCE.Write(writer, value.Value)
}

type FfiDestroyerParsedHeader struct{}

func (_ FfiDestroyerParsedHeader) Destroy(value ParsedHeader) {
	value.Destroy()
}

// Result of parsing an RFC 5322 / MIME message.
type ParsedMessage struct {
	From            *string
	To              []string
	Cc              []string
	Bcc             []string
	Subject         *string
	MessageId       *string
	DateUnixSeconds *int64
	BodyText        string
	BodyHtml        *string
	Headers         []ParsedHeader
	MimeParts       []ParsedMimePart
}

func (r *ParsedMessage) Destroy() {
	FfiDestroyerOptionalString{}.Destroy(r.From)
	FfiDestroyerSequenceString{}.Destroy(r.To)
	FfiDestroyerSequenceString{}.Destroy(r.Cc)
	FfiDestroyerSequenceString{}.Destroy(r.Bcc)
	FfiDestroyerOptionalString{}.Destroy(r.Subject)
	FfiDestroyerOptionalString{}.Destroy(r.MessageId)
	FfiDestroyerOptionalInt64{}.Destroy(r.DateUnixSeconds)
	FfiDestroyerString{}.Destroy(r.BodyText)
	FfiDestroyerOptionalString{}.Destroy(r.BodyHtml)
	FfiDestroyerSequenceParsedHeader{}.Destroy(r.Headers)
	FfiDestroyerSequenceParsedMimePart{}.Destroy(r.MimeParts)
}

type FfiConverterParsedMessage struct{}

var FfiConverterParsedMessageINSTANCE = FfiConverterParsedMessage{}

func (c FfiConverterParsedMessage) Lift(rb RustBufferI) ParsedMessage {
	return LiftFromRustBuffer[ParsedMessage](c, rb)
}

func (c FfiConverterParsedMessage) Read(reader io.Reader) ParsedMessage {
	return ParsedMessage{
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterSequenceStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalInt64INSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterSequenceParsedHeaderINSTANCE.Read(reader),
		FfiConverterSequenceParsedMimePartINSTANCE.Read(reader),
	}
}

func (c FfiConverterParsedMessage) Lower(value ParsedMessage) C.RustBuffer {
	return LowerIntoRustBuffer[ParsedMessage](c, value)
}

func (c FfiConverterParsedMessage) LowerExternal(value ParsedMessage) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ParsedMessage](c, value))
}

func (c FfiConverterParsedMessage) Write(writer io.Writer, value ParsedMessage) {
	FfiConverterOptionalStringINSTANCE.Write(writer, value.From)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.To)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.Cc)
	FfiConverterSequenceStringINSTANCE.Write(writer, value.Bcc)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Subject)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.MessageId)
	FfiConverterOptionalInt64INSTANCE.Write(writer, value.DateUnixSeconds)
	FfiConverterStringINSTANCE.Write(writer, value.BodyText)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.BodyHtml)
	FfiConverterSequenceParsedHeaderINSTANCE.Write(writer, value.Headers)
	FfiConverterSequenceParsedMimePartINSTANCE.Write(writer, value.MimeParts)
}

type FfiDestroyerParsedMessage struct{}

func (_ FfiDestroyerParsedMessage) Destroy(value ParsedMessage) {
	value.Destroy()
}

type ParsedMimePart struct {
	ContentType string
	Disposition *string
	Filename    *string
	SizeBytes   uint64
}

func (r *ParsedMimePart) Destroy() {
	FfiDestroyerString{}.Destroy(r.ContentType)
	FfiDestroyerOptionalString{}.Destroy(r.Disposition)
	FfiDestroyerOptionalString{}.Destroy(r.Filename)
	FfiDestroyerUint64{}.Destroy(r.SizeBytes)
}

type FfiConverterParsedMimePart struct{}

var FfiConverterParsedMimePartINSTANCE = FfiConverterParsedMimePart{}

func (c FfiConverterParsedMimePart) Lift(rb RustBufferI) ParsedMimePart {
	return LiftFromRustBuffer[ParsedMimePart](c, rb)
}

func (c FfiConverterParsedMimePart) Read(reader io.Reader) ParsedMimePart {
	return ParsedMimePart{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
	}
}

func (c FfiConverterParsedMimePart) Lower(value ParsedMimePart) C.RustBuffer {
	return LowerIntoRustBuffer[ParsedMimePart](c, value)
}

func (c FfiConverterParsedMimePart) LowerExternal(value ParsedMimePart) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ParsedMimePart](c, value))
}

func (c FfiConverterParsedMimePart) Write(writer io.Writer, value ParsedMimePart) {
	FfiConverterStringINSTANCE.Write(writer, value.ContentType)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Disposition)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.Filename)
	FfiConverterUint64INSTANCE.Write(writer, value.SizeBytes)
}

type FfiDestroyerParsedMimePart struct{}

func (_ FfiDestroyerParsedMimePart) Destroy(value ParsedMimePart) {
	value.Destroy()
}

// Per-transaction context the bridge folds into the `Received:` header. See
// RFC 5321 §4.4 / RFC 5322 §3.6.7; TLS-extension fields per RFC 8314 §4.1.
//
// `server_hostname`, `tls_version`, `tls_cipher`, `queue_id` are bridge-owned
// (not sender-controlled). `helo_domain` and `client_ip` are sender-controlled
// and sanitized via [`safe_token`] — a hostile EHLO carrying `\r\n` cannot forge
// a second header line.
type ReceivedHeaderOpts struct {
	// Our receiving host for the `by` clause — the bridge's first local
	// (mail-hosting) domain or OS hostname. Empty / unsafe ⇒ `fauna-bridge.invalid`.
	ServerHostname string
	// EHLO/HELO the sender announced; sanitized to `unknown` if it carries any
	// CR / LF / non-printable byte.
	HeloDomain string
	// Remote peer IP (no port); sanitized to `unknown` likewise.
	ClientIp string
	// TLS protocol version, e.g. `TLS1.3`; empty ⇒ cleartext (`with ESMTP`,
	// no cipher parenthetical). Non-empty ⇒ `with ESMTPS (<ver> <cipher>)`.
	TlsVersion string
	// TLS cipher suite name, e.g. `TLS_AES_256_GCM_SHA384`. Ignored when
	// `tls_version` is empty.
	TlsCipher string
	// Short opaque transaction id for the `id` clause (Go mints it with
	// crypto/rand — that's the I/O half). Empty ⇒ a fixed all-zero placeholder.
	QueueId string
	// Single-recipient envelope address for the `for <addr>;` clause. Empty for
	// a multi-recipient delivery — the one prepended header is sealed to every
	// recipient, so emitting `for` would leak cross-recipient correlation.
	Recipient string
}

func (r *ReceivedHeaderOpts) Destroy() {
	FfiDestroyerString{}.Destroy(r.ServerHostname)
	FfiDestroyerString{}.Destroy(r.HeloDomain)
	FfiDestroyerString{}.Destroy(r.ClientIp)
	FfiDestroyerString{}.Destroy(r.TlsVersion)
	FfiDestroyerString{}.Destroy(r.TlsCipher)
	FfiDestroyerString{}.Destroy(r.QueueId)
	FfiDestroyerString{}.Destroy(r.Recipient)
}

type FfiConverterReceivedHeaderOpts struct{}

var FfiConverterReceivedHeaderOptsINSTANCE = FfiConverterReceivedHeaderOpts{}

func (c FfiConverterReceivedHeaderOpts) Lift(rb RustBufferI) ReceivedHeaderOpts {
	return LiftFromRustBuffer[ReceivedHeaderOpts](c, rb)
}

func (c FfiConverterReceivedHeaderOpts) Read(reader io.Reader) ReceivedHeaderOpts {
	return ReceivedHeaderOpts{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterReceivedHeaderOpts) Lower(value ReceivedHeaderOpts) C.RustBuffer {
	return LowerIntoRustBuffer[ReceivedHeaderOpts](c, value)
}

func (c FfiConverterReceivedHeaderOpts) LowerExternal(value ReceivedHeaderOpts) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ReceivedHeaderOpts](c, value))
}

func (c FfiConverterReceivedHeaderOpts) Write(writer io.Writer, value ReceivedHeaderOpts) {
	FfiConverterStringINSTANCE.Write(writer, value.ServerHostname)
	FfiConverterStringINSTANCE.Write(writer, value.HeloDomain)
	FfiConverterStringINSTANCE.Write(writer, value.ClientIp)
	FfiConverterStringINSTANCE.Write(writer, value.TlsVersion)
	FfiConverterStringINSTANCE.Write(writer, value.TlsCipher)
	FfiConverterStringINSTANCE.Write(writer, value.QueueId)
	FfiConverterStringINSTANCE.Write(writer, value.Recipient)
}

type FfiDestroyerReceivedHeaderOpts struct{}

func (_ FfiDestroyerReceivedHeaderOpts) Destroy(value ReceivedHeaderOpts) {
	value.Destroy()
}

// Scan policy projected from nest config (the bridge passes it in).
type ScanPolicy struct {
	ClamavEnabled          bool
	ClamavActionOnInfected ClamavAction
	RspamdEnabled          bool
	// rspamd raw→scaled multiplier × 1000 (default 500 = 0.5 — maps rspamd's
	// nominal 30 to our 15).
	RspamdScoreScalingPerMille uint16
}

func (r *ScanPolicy) Destroy() {
	FfiDestroyerBool{}.Destroy(r.ClamavEnabled)
	FfiDestroyerClamavAction{}.Destroy(r.ClamavActionOnInfected)
	FfiDestroyerBool{}.Destroy(r.RspamdEnabled)
	FfiDestroyerUint16{}.Destroy(r.RspamdScoreScalingPerMille)
}

type FfiConverterScanPolicy struct{}

var FfiConverterScanPolicyINSTANCE = FfiConverterScanPolicy{}

func (c FfiConverterScanPolicy) Lift(rb RustBufferI) ScanPolicy {
	return LiftFromRustBuffer[ScanPolicy](c, rb)
}

func (c FfiConverterScanPolicy) Read(reader io.Reader) ScanPolicy {
	return ScanPolicy{
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterClamavActionINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterUint16INSTANCE.Read(reader),
	}
}

func (c FfiConverterScanPolicy) Lower(value ScanPolicy) C.RustBuffer {
	return LowerIntoRustBuffer[ScanPolicy](c, value)
}

func (c FfiConverterScanPolicy) LowerExternal(value ScanPolicy) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ScanPolicy](c, value))
}

func (c FfiConverterScanPolicy) Write(writer io.Writer, value ScanPolicy) {
	FfiConverterBoolINSTANCE.Write(writer, value.ClamavEnabled)
	FfiConverterClamavActionINSTANCE.Write(writer, value.ClamavActionOnInfected)
	FfiConverterBoolINSTANCE.Write(writer, value.RspamdEnabled)
	FfiConverterUint16INSTANCE.Write(writer, value.RspamdScoreScalingPerMille)
}

type FfiDestroyerScanPolicy struct{}

func (_ FfiDestroyerScanPolicy) Destroy(value ScanPolicy) {
	value.Destroy()
}

// The sealed bytes the Go MDA ships over the caller-scoped WS-RPC scheduling
// rail to deliver one iMIP to a mailbox-less Fauna attendee. UniFFI record in
// the `fauna_mail` namespace; the Go MDA passes each field straight to the
// matching RPC.
type SealedSchedulingDelivery struct {
	// The MLS Welcome → `welcome.deliver` (tagged `Scheduling` so the recipient
	// routes the channel to calendar-apply, never the chat UI).
	WelcomeBytes []byte
	// The one-off channel id (hex) → both `welcome.deliver` and `channel.send`.
	ChannelIdHex string
	// The first (and only) application-message envelope → `channel.send`.
	AppEnvelope []byte
}

func (r *SealedSchedulingDelivery) Destroy() {
	FfiDestroyerBytes{}.Destroy(r.WelcomeBytes)
	FfiDestroyerString{}.Destroy(r.ChannelIdHex)
	FfiDestroyerBytes{}.Destroy(r.AppEnvelope)
}

type FfiConverterSealedSchedulingDelivery struct{}

var FfiConverterSealedSchedulingDeliveryINSTANCE = FfiConverterSealedSchedulingDelivery{}

func (c FfiConverterSealedSchedulingDelivery) Lift(rb RustBufferI) SealedSchedulingDelivery {
	return LiftFromRustBuffer[SealedSchedulingDelivery](c, rb)
}

func (c FfiConverterSealedSchedulingDelivery) Read(reader io.Reader) SealedSchedulingDelivery {
	return SealedSchedulingDelivery{
		FfiConverterBytesINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
	}
}

func (c FfiConverterSealedSchedulingDelivery) Lower(value SealedSchedulingDelivery) C.RustBuffer {
	return LowerIntoRustBuffer[SealedSchedulingDelivery](c, value)
}

func (c FfiConverterSealedSchedulingDelivery) LowerExternal(value SealedSchedulingDelivery) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[SealedSchedulingDelivery](c, value))
}

func (c FfiConverterSealedSchedulingDelivery) Write(writer io.Writer, value SealedSchedulingDelivery) {
	FfiConverterBytesINSTANCE.Write(writer, value.WelcomeBytes)
	FfiConverterStringINSTANCE.Write(writer, value.ChannelIdHex)
	FfiConverterBytesINSTANCE.Write(writer, value.AppEnvelope)
}

type FfiDestroyerSealedSchedulingDelivery struct{}

func (_ FfiDestroyerSealedSchedulingDelivery) Destroy(value SealedSchedulingDelivery) {
	value.Destroy()
}

// Score thresholds passed in by the bridge (projected from nest config at
// startup). Thresholds are integer **points** on the 0–15 combined-score
// scale. **`0 = disabled`** for that tier — the tier's action never fires.
//
// Default is the permissive auto-Junk policy: spam_folder=5 (→ Junk),
// reject=0 (off). When both tiers are non-zero, `spam_folder < reject` must
// hold.
type SpamPolicy struct {
	// Combined score (points) >= this → `AcceptToSpamFolder` (Junk). `0` = no
	// auto-Junk (everything below the reject tier lands in INBOX).
	SpamFolderThreshold uint32
	// Combined score (points) >= this → `Reject` (550 at the MTA). `0` = off
	// (the default; admin opts in by setting a non-zero value).
	RejectThreshold uint32
	// If true, a DMARC Quarantine-policy fail short-circuits to `PolicyJunk`
	// regardless of the score — honoring the *sender's* published `p=quarantine`
	// DMARC policy (the word is the DMARC standard's; the mail lands in Junk).
	//
	// DMARC *reject* (`p=reject`) is NOT honored here — it is enforced one
	// stage earlier as a 550 5.7.1 by the bridge's auth-enforce gate
	// (`bins/fauna-bridges/internal/mta/auth_enforce.go`), which also
	// honors `LogOnly`.
	HonorDmarcQuarantine bool
}

func (r *SpamPolicy) Destroy() {
	FfiDestroyerUint32{}.Destroy(r.SpamFolderThreshold)
	FfiDestroyerUint32{}.Destroy(r.RejectThreshold)
	FfiDestroyerBool{}.Destroy(r.HonorDmarcQuarantine)
}

type FfiConverterSpamPolicy struct{}

var FfiConverterSpamPolicyINSTANCE = FfiConverterSpamPolicy{}

func (c FfiConverterSpamPolicy) Lift(rb RustBufferI) SpamPolicy {
	return LiftFromRustBuffer[SpamPolicy](c, rb)
}

func (c FfiConverterSpamPolicy) Read(reader io.Reader) SpamPolicy {
	return SpamPolicy{
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterSpamPolicy) Lower(value SpamPolicy) C.RustBuffer {
	return LowerIntoRustBuffer[SpamPolicy](c, value)
}

func (c FfiConverterSpamPolicy) LowerExternal(value SpamPolicy) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[SpamPolicy](c, value))
}

func (c FfiConverterSpamPolicy) Write(writer io.Writer, value SpamPolicy) {
	FfiConverterUint32INSTANCE.Write(writer, value.SpamFolderThreshold)
	FfiConverterUint32INSTANCE.Write(writer, value.RejectThreshold)
	FfiConverterBoolINSTANCE.Write(writer, value.HonorDmarcQuarantine)
}

type FfiDestroyerSpamPolicy struct{}

func (_ FfiDestroyerSpamPolicy) Destroy(value SpamPolicy) {
	value.Destroy()
}

// The result of [`apply_spam_training`]: the mutated model to re-seal + the
// forward n-gram delta to seal into the training-history row.
type SpamTrainingMutation struct {
	// The mutated model re-serialized ([`SpamModel::to_bytes`]) — the MDA re-seals
	// these to the actor's **own** recipient key and writes them back opaque via
	// `fauna.bridges.put_spam_model` (leg 2 write-back).
	NewModelBytes []byte
	// `serde_json` of the distinct forward n-gram set this event touched — the same
	// bytes a *server-written* `spam_training_history.model_delta_applied` stores
	// plaintext; the MDA seals these into the history row so a later client-side
	// undo can replay the exact inverse (`ModelWriteOp::Undo`). An empty set ⇒ `[]`.
	DeltaJson []byte
}

func (r *SpamTrainingMutation) Destroy() {
	FfiDestroyerBytes{}.Destroy(r.NewModelBytes)
	FfiDestroyerBytes{}.Destroy(r.DeltaJson)
}

type FfiConverterSpamTrainingMutation struct{}

var FfiConverterSpamTrainingMutationINSTANCE = FfiConverterSpamTrainingMutation{}

func (c FfiConverterSpamTrainingMutation) Lift(rb RustBufferI) SpamTrainingMutation {
	return LiftFromRustBuffer[SpamTrainingMutation](c, rb)
}

func (c FfiConverterSpamTrainingMutation) Read(reader io.Reader) SpamTrainingMutation {
	return SpamTrainingMutation{
		FfiConverterBytesINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
	}
}

func (c FfiConverterSpamTrainingMutation) Lower(value SpamTrainingMutation) C.RustBuffer {
	return LowerIntoRustBuffer[SpamTrainingMutation](c, value)
}

func (c FfiConverterSpamTrainingMutation) LowerExternal(value SpamTrainingMutation) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[SpamTrainingMutation](c, value))
}

func (c FfiConverterSpamTrainingMutation) Write(writer io.Writer, value SpamTrainingMutation) {
	FfiConverterBytesINSTANCE.Write(writer, value.NewModelBytes)
	FfiConverterBytesINSTANCE.Write(writer, value.DeltaJson)
}

type FfiDestroyerSpamTrainingMutation struct{}

func (_ FfiDestroyerSpamTrainingMutation) Destroy(value SpamTrainingMutation) {
	value.Destroy()
}

// A freshly sealed staging payload: the one-shot key (to ride the RPC beside
// the reference) and the sealed bytes (nonce-prefixed ciphertext, ready for
// [`split_sealed_mail_body`] → upload).
type StagedSeal struct {
	// The one-shot random key, [`STAGING_KEY_BYTES`] long.
	Key []byte
	// Nonce-prefixed ciphertext — what actually gets chunked and staged.
	Sealed []byte
}

func (r *StagedSeal) Destroy() {
	FfiDestroyerBytes{}.Destroy(r.Key)
	FfiDestroyerBytes{}.Destroy(r.Sealed)
}

type FfiConverterStagedSeal struct{}

var FfiConverterStagedSealINSTANCE = FfiConverterStagedSeal{}

func (c FfiConverterStagedSeal) Lift(rb RustBufferI) StagedSeal {
	return LiftFromRustBuffer[StagedSeal](c, rb)
}

func (c FfiConverterStagedSeal) Read(reader io.Reader) StagedSeal {
	return StagedSeal{
		FfiConverterBytesINSTANCE.Read(reader),
		FfiConverterBytesINSTANCE.Read(reader),
	}
}

func (c FfiConverterStagedSeal) Lower(value StagedSeal) C.RustBuffer {
	return LowerIntoRustBuffer[StagedSeal](c, value)
}

func (c FfiConverterStagedSeal) LowerExternal(value StagedSeal) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[StagedSeal](c, value))
}

func (c FfiConverterStagedSeal) Write(writer io.Writer, value StagedSeal) {
	FfiConverterBytesINSTANCE.Write(writer, value.Key)
	FfiConverterBytesINSTANCE.Write(writer, value.Sealed)
}

type FfiDestroyerStagedSeal struct{}

func (_ FfiDestroyerStagedSeal) Destroy(value StagedSeal) {
	value.Destroy()
}

// One stored filter row to evaluate. Mirrors `fauna_protocol::email::EmailFilter`
// minus the storage-internal `owner`/`name`/`created_at` (irrelevant to the
// decision). `priority`/`id` give the deterministic evaluation order
// (`email.rs` DB: `ORDER BY priority ASC, id ASC`).
type StoredFilter struct {
	Id          int64
	Conditions  []FilterCondition
	Combination FilterCombination
	Action      FilterAction
	Priority    int32
	// When `false` (the default) a match is **terminal** — first-match-wins
	// stops here. When `true` (Sieve `continue`) the action is recorded and
	// evaluation falls through to later rules, so several rules' actions can
	// apply in order (`smtp-server.md` § Email filter rules — multi-action).
	ContinueOnMatch bool
}

func (r *StoredFilter) Destroy() {
	FfiDestroyerInt64{}.Destroy(r.Id)
	FfiDestroyerSequenceFilterCondition{}.Destroy(r.Conditions)
	FfiDestroyerFilterCombination{}.Destroy(r.Combination)
	FfiDestroyerFilterAction{}.Destroy(r.Action)
	FfiDestroyerInt32{}.Destroy(r.Priority)
	FfiDestroyerBool{}.Destroy(r.ContinueOnMatch)
}

type FfiConverterStoredFilter struct{}

var FfiConverterStoredFilterINSTANCE = FfiConverterStoredFilter{}

func (c FfiConverterStoredFilter) Lift(rb RustBufferI) StoredFilter {
	return LiftFromRustBuffer[StoredFilter](c, rb)
}

func (c FfiConverterStoredFilter) Read(reader io.Reader) StoredFilter {
	return StoredFilter{
		FfiConverterInt64INSTANCE.Read(reader),
		FfiConverterSequenceFilterConditionINSTANCE.Read(reader),
		FfiConverterFilterCombinationINSTANCE.Read(reader),
		FfiConverterFilterActionINSTANCE.Read(reader),
		FfiConverterInt32INSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterStoredFilter) Lower(value StoredFilter) C.RustBuffer {
	return LowerIntoRustBuffer[StoredFilter](c, value)
}

func (c FfiConverterStoredFilter) LowerExternal(value StoredFilter) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[StoredFilter](c, value))
}

func (c FfiConverterStoredFilter) Write(writer io.Writer, value StoredFilter) {
	FfiConverterInt64INSTANCE.Write(writer, value.Id)
	FfiConverterSequenceFilterConditionINSTANCE.Write(writer, value.Conditions)
	FfiConverterFilterCombinationINSTANCE.Write(writer, value.Combination)
	FfiConverterFilterActionINSTANCE.Write(writer, value.Action)
	FfiConverterInt32INSTANCE.Write(writer, value.Priority)
	FfiConverterBoolINSTANCE.Write(writer, value.ContinueOnMatch)
}

type FfiDestroyerStoredFilter struct{}

func (_ FfiDestroyerStoredFilter) Destroy(value StoredFilter) {
	value.Destroy()
}

// UniFFI mirror of `fauna_core::ical::AttendeeInfo` (writer attendee input).
type WriterAttendeeInfo struct {
	Name        string
	Email       string
	Partstat    string
	FaunaStatus string
}

func (r *WriterAttendeeInfo) Destroy() {
	FfiDestroyerString{}.Destroy(r.Name)
	FfiDestroyerString{}.Destroy(r.Email)
	FfiDestroyerString{}.Destroy(r.Partstat)
	FfiDestroyerString{}.Destroy(r.FaunaStatus)
}

type FfiConverterWriterAttendeeInfo struct{}

var FfiConverterWriterAttendeeInfoINSTANCE = FfiConverterWriterAttendeeInfo{}

func (c FfiConverterWriterAttendeeInfo) Lift(rb RustBufferI) WriterAttendeeInfo {
	return LiftFromRustBuffer[WriterAttendeeInfo](c, rb)
}

func (c FfiConverterWriterAttendeeInfo) Read(reader io.Reader) WriterAttendeeInfo {
	return WriterAttendeeInfo{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterWriterAttendeeInfo) Lower(value WriterAttendeeInfo) C.RustBuffer {
	return LowerIntoRustBuffer[WriterAttendeeInfo](c, value)
}

func (c FfiConverterWriterAttendeeInfo) LowerExternal(value WriterAttendeeInfo) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[WriterAttendeeInfo](c, value))
}

func (c FfiConverterWriterAttendeeInfo) Write(writer io.Writer, value WriterAttendeeInfo) {
	FfiConverterStringINSTANCE.Write(writer, value.Name)
	FfiConverterStringINSTANCE.Write(writer, value.Email)
	FfiConverterStringINSTANCE.Write(writer, value.Partstat)
	FfiConverterStringINSTANCE.Write(writer, value.FaunaStatus)
}

type FfiDestroyerWriterAttendeeInfo struct{}

func (_ FfiDestroyerWriterAttendeeInfo) Destroy(value WriterAttendeeInfo) {
	value.Destroy()
}

// UniFFI mirror of `fauna_core::ical::EventFields` (the iCalendar writer's
// event input), defined in the `fauna_mail` namespace so the Go MDA binding
// resolves. Field-for-field identical; the `From` impl below is an exhaustive
// destructure→construct, so a new `fauna_core::ical::EventFields` field is a
// compile error here until it is mirrored.
type WriterEventFields struct {
	Summary      string
	Dtstart      string
	Dtend        string
	Duration     string
	Location     string
	Geo          string
	Url          string
	Rrule        string
	Exdates      string
	Categories   string
	Status       string
	Uid          string
	Sequence     uint32
	Alarm        string
	Description  string
	RecurrenceId string
	IsAllDay     bool
}

func (r *WriterEventFields) Destroy() {
	FfiDestroyerString{}.Destroy(r.Summary)
	FfiDestroyerString{}.Destroy(r.Dtstart)
	FfiDestroyerString{}.Destroy(r.Dtend)
	FfiDestroyerString{}.Destroy(r.Duration)
	FfiDestroyerString{}.Destroy(r.Location)
	FfiDestroyerString{}.Destroy(r.Geo)
	FfiDestroyerString{}.Destroy(r.Url)
	FfiDestroyerString{}.Destroy(r.Rrule)
	FfiDestroyerString{}.Destroy(r.Exdates)
	FfiDestroyerString{}.Destroy(r.Categories)
	FfiDestroyerString{}.Destroy(r.Status)
	FfiDestroyerString{}.Destroy(r.Uid)
	FfiDestroyerUint32{}.Destroy(r.Sequence)
	FfiDestroyerString{}.Destroy(r.Alarm)
	FfiDestroyerString{}.Destroy(r.Description)
	FfiDestroyerString{}.Destroy(r.RecurrenceId)
	FfiDestroyerBool{}.Destroy(r.IsAllDay)
}

type FfiConverterWriterEventFields struct{}

var FfiConverterWriterEventFieldsINSTANCE = FfiConverterWriterEventFields{}

func (c FfiConverterWriterEventFields) Lift(rb RustBufferI) WriterEventFields {
	return LiftFromRustBuffer[WriterEventFields](c, rb)
}

func (c FfiConverterWriterEventFields) Read(reader io.Reader) WriterEventFields {
	return WriterEventFields{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterWriterEventFields) Lower(value WriterEventFields) C.RustBuffer {
	return LowerIntoRustBuffer[WriterEventFields](c, value)
}

func (c FfiConverterWriterEventFields) LowerExternal(value WriterEventFields) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[WriterEventFields](c, value))
}

func (c FfiConverterWriterEventFields) Write(writer io.Writer, value WriterEventFields) {
	FfiConverterStringINSTANCE.Write(writer, value.Summary)
	FfiConverterStringINSTANCE.Write(writer, value.Dtstart)
	FfiConverterStringINSTANCE.Write(writer, value.Dtend)
	FfiConverterStringINSTANCE.Write(writer, value.Duration)
	FfiConverterStringINSTANCE.Write(writer, value.Location)
	FfiConverterStringINSTANCE.Write(writer, value.Geo)
	FfiConverterStringINSTANCE.Write(writer, value.Url)
	FfiConverterStringINSTANCE.Write(writer, value.Rrule)
	FfiConverterStringINSTANCE.Write(writer, value.Exdates)
	FfiConverterStringINSTANCE.Write(writer, value.Categories)
	FfiConverterStringINSTANCE.Write(writer, value.Status)
	FfiConverterStringINSTANCE.Write(writer, value.Uid)
	FfiConverterUint32INSTANCE.Write(writer, value.Sequence)
	FfiConverterStringINSTANCE.Write(writer, value.Alarm)
	FfiConverterStringINSTANCE.Write(writer, value.Description)
	FfiConverterStringINSTANCE.Write(writer, value.RecurrenceId)
	FfiConverterBoolINSTANCE.Write(writer, value.IsAllDay)
}

type FfiDestroyerWriterEventFields struct{}

func (_ FfiDestroyerWriterEventFields) Destroy(value WriterEventFields) {
	value.Destroy()
}

type AuthError struct {
	err error
}

// Convenience method to turn *AuthError into error
// Avoiding treating nil pointer as non nil error interface
func (err *AuthError) AsError() error {
	if err == nil {
		return nil
	} else {
		return err
	}
}

func (err AuthError) Error() string {
	return fmt.Sprintf("AuthError: %s", err.err.Error())
}

func (err AuthError) Unwrap() error {
	return err.err
}

// Err* are used for checking error type with `errors.Is`
var ErrAuthErrorUnparseable = fmt.Errorf("AuthErrorUnparseable")
var ErrAuthErrorResolverInit = fmt.Errorf("AuthErrorResolverInit")

// Variant structs
type AuthErrorUnparseable struct {
}

func NewAuthErrorUnparseable() *AuthError {
	return &AuthError{err: &AuthErrorUnparseable{}}
}

func (e AuthErrorUnparseable) destroy() {
}

func (err AuthErrorUnparseable) Error() string {
	return fmt.Sprint("Unparseable")
}

func (self AuthErrorUnparseable) Is(target error) bool {
	return target == ErrAuthErrorUnparseable
}

type AuthErrorResolverInit struct {
	Field0 string
}

func NewAuthErrorResolverInit(
	var0 string,
) *AuthError {
	return &AuthError{err: &AuthErrorResolverInit{
		Field0: var0}}
}

func (e AuthErrorResolverInit) destroy() {
	FfiDestroyerString{}.Destroy(e.Field0)
}

func (err AuthErrorResolverInit) Error() string {
	return fmt.Sprint("ResolverInit",
		": ",

		"Field0=",
		err.Field0,
	)
}

func (self AuthErrorResolverInit) Is(target error) bool {
	return target == ErrAuthErrorResolverInit
}

type FfiConverterAuthError struct{}

var FfiConverterAuthErrorINSTANCE = FfiConverterAuthError{}

func (c FfiConverterAuthError) Lift(eb RustBufferI) *AuthError {
	return LiftFromRustBuffer[*AuthError](c, eb)
}

func (c FfiConverterAuthError) Lower(value *AuthError) C.RustBuffer {
	return LowerIntoRustBuffer[*AuthError](c, value)
}

func (c FfiConverterAuthError) LowerExternal(value *AuthError) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*AuthError](c, value))
}

func (c FfiConverterAuthError) Read(reader io.Reader) *AuthError {
	errorID := readUint32(reader)

	switch errorID {
	case 1:
		return &AuthError{&AuthErrorUnparseable{}}
	case 2:
		return &AuthError{&AuthErrorResolverInit{
			Field0: FfiConverterStringINSTANCE.Read(reader),
		}}
	default:
		panic(fmt.Sprintf("Unknown error code %d in FfiConverterAuthError.Read()", errorID))
	}
}

func (c FfiConverterAuthError) Write(writer io.Writer, value *AuthError) {
	switch variantValue := value.err.(type) {
	case *AuthErrorUnparseable:
		writeInt32(writer, 1)
	case *AuthErrorResolverInit:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Field0)
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiConverterAuthError.Write", value))
	}
}

type FfiDestroyerAuthError struct{}

func (_ FfiDestroyerAuthError) Destroy(value *AuthError) {
	switch variantValue := value.err.(type) {
	case AuthErrorUnparseable:
		variantValue.destroy()
	case AuthErrorResolverInit:
		variantValue.destroy()
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiDestroyerAuthError.Destroy", value))
	}
}

// The decision for a matched [`FilterAction::AutoReply`] (Sieve `vacation`),
// evaluated at the MTA perimeter on the plaintext-floor envelope + headers
// (`smtp-server.md` § Email filter rules — the same storage-mode reason filters
// evaluate here, not in nest). `Send` means proceed to the rate-limit claim;
// every other variant is a loop-guard suppression and doubles as a stable
// metric label (`smtp_inbound_autoreply_suppressed_total{reason}`). The
// rate-limit itself is a separate, stateful check (`auto_reply_log`) the caller
// makes after `Send`.
type AutoReplyGate uint

const (
	// No loop-guard fired — proceed to the rate-limit claim.
	AutoReplyGateSend AutoReplyGate = 1
	// Envelope sender is null (`MAIL FROM: <>`) — it's a bounce (RFC 3834).
	AutoReplyGateSuppressNullSender AutoReplyGate = 2
	// `Auto-Submitted` header ≠ `no` — the message is itself automated (RFC 3834).
	AutoReplyGateSuppressAutoSubmitted AutoReplyGate = 3
	// `List-*` header or `Precedence: bulk|list|junk` — list/bulk mail
	// (RFC 5230 §4.6, see `mail-mass-mailing.md`).
	AutoReplyGateSuppressBulk AutoReplyGate = 4
	// Envelope sender is one of our own `local_domains` — don't auto-reply to
	// our own notifications / postmaster traffic.
	AutoReplyGateSuppressOwnDomain AutoReplyGate = 5
	// Recipient is not in the message `To`/`Cc` (RFC 5230 §4.4 — guards
	// bcc/list traffic).
	AutoReplyGateSuppressNotInRecipients AutoReplyGate = 6
)

type FfiConverterAutoReplyGate struct{}

var FfiConverterAutoReplyGateINSTANCE = FfiConverterAutoReplyGate{}

func (c FfiConverterAutoReplyGate) Lift(rb RustBufferI) AutoReplyGate {
	return LiftFromRustBuffer[AutoReplyGate](c, rb)
}

func (c FfiConverterAutoReplyGate) Lower(value AutoReplyGate) C.RustBuffer {
	return LowerIntoRustBuffer[AutoReplyGate](c, value)
}

func (c FfiConverterAutoReplyGate) LowerExternal(value AutoReplyGate) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[AutoReplyGate](c, value))
}
func (FfiConverterAutoReplyGate) Read(reader io.Reader) AutoReplyGate {
	id := readInt32(reader)
	return AutoReplyGate(id)
}

func (FfiConverterAutoReplyGate) Write(writer io.Writer, value AutoReplyGate) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerAutoReplyGate struct{}

func (_ FfiDestroyerAutoReplyGate) Destroy(value AutoReplyGate) {
}

// Admin action when ClamAV finds malware (`mail.scanning.clamav_action_on_infected`).
type ClamavAction uint

const (
	// Default — `554 5.7.1` at the SMTP perimeter; message never stored.
	ClamavActionReject ClamavAction = 1
	// Deliver to the recipient's Junk (still records the row).
	ClamavActionJunk ClamavAction = 2
	// Deliver with `X-Fauna-Scan-Clamav: infected` headers; no routing override.
	ClamavActionTag ClamavAction = 3
)

type FfiConverterClamavAction struct{}

var FfiConverterClamavActionINSTANCE = FfiConverterClamavAction{}

func (c FfiConverterClamavAction) Lift(rb RustBufferI) ClamavAction {
	return LiftFromRustBuffer[ClamavAction](c, rb)
}

func (c FfiConverterClamavAction) Lower(value ClamavAction) C.RustBuffer {
	return LowerIntoRustBuffer[ClamavAction](c, value)
}

func (c FfiConverterClamavAction) LowerExternal(value ClamavAction) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ClamavAction](c, value))
}
func (FfiConverterClamavAction) Read(reader io.Reader) ClamavAction {
	id := readInt32(reader)
	return ClamavAction(id)
}

func (FfiConverterClamavAction) Write(writer io.Writer, value ClamavAction) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerClamavAction struct{}

func (_ FfiDestroyerClamavAction) Destroy(value ClamavAction) {
}

// The export file format — the wizard's step-1 choice.
//
// Three arms, no more, no less in v1 (§ Architectural rules).
//
// **The one definition** (collapsed 2026-09-21, with the client drive loop).
// `fauna_client_mail_settings::export::ExportFormat` is a plain `pub use` of
// this type, not a parallel declaration: one concept gets one definition
// (priority #1/#2), and the layering puts it here — that crate depends on
// this one, never the reverse.
//
// The serde + UniFFI derives therefore live on **this** declaration rather
// than on the re-export, because a `pub use` carries no derives of its own:
// without them here the FFI face would compile with the type silently
// missing, the same trap `fauna-mail`'s own `uniffi` feature comment records
// for the mail-auth verdict types. A consumer that wants the FFI face enables
// this crate's `uniffi` feature; `fauna-client-mail-settings` forwards it.
//
// **Never reintroduce a client-side twin with a `From` between them** — a
// conversion is exactly what would let the divergence grow back.
type ExportFormat uint

const (
	// RFC 4155 — one text file per mailbox. Broadest MUA support.
	ExportFormatMbox ExportFormat = 1
	// The Courier Maildir++ extension — one file per message in a
	// `cur/`/`new/`/`tmp/` tree per mailbox.
	ExportFormatMaildirPlus ExportFormat = 2
	// One `.eml` per message in a flat directory plus a metadata manifest.
	ExportFormatEmlZip ExportFormat = 3
)

type FfiConverterExportFormat struct{}

var FfiConverterExportFormatINSTANCE = FfiConverterExportFormat{}

func (c FfiConverterExportFormat) Lift(rb RustBufferI) ExportFormat {
	return LiftFromRustBuffer[ExportFormat](c, rb)
}

func (c FfiConverterExportFormat) Lower(value ExportFormat) C.RustBuffer {
	return LowerIntoRustBuffer[ExportFormat](c, value)
}

func (c FfiConverterExportFormat) LowerExternal(value ExportFormat) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ExportFormat](c, value))
}
func (FfiConverterExportFormat) Read(reader io.Reader) ExportFormat {
	id := readInt32(reader)
	return ExportFormat(id)
}

func (FfiConverterExportFormat) Write(writer io.Writer, value ExportFormat) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerExportFormat struct{}

func (_ FfiDestroyerExportFormat) Destroy(value ExportFormat) {
}

// What to do when a filter matches. Mirrors
// `fauna_protocol::email::EmailFilterAction`.
//
// The placement actions ([`FilterAction::FileInto`], [`FilterAction::AddLabel`],
// [`FilterAction::Discard`], [`FilterAction::Allow`]) and [`FilterAction::Reject`]
// are wired at the Go MTA perimeter; [`FilterAction::Forward`] is owned by
// `mail-forwarding.md`; [`FilterAction::AutoReply`] is wired via the perimeter
// loop-guard ([`auto_reply_decision`]) + compose
// ([`crate::outbound::autoreply::compose_auto_reply`]) → sign →
// `fauna.bridges.send_auto_reply` (atomic rate-limit + null-sender enqueue). The
// evaluator returns the matched action regardless — composition + side effects
// are the caller's job (`smtp-server.md` § Email filter rules spells out the
// precedence).
type FilterAction interface {
	Destroy()
}

// Whitelist: deliver to INBOX, overriding any spam-disposition.
type FilterActionAllow struct {
}

func (e FilterActionAllow) Destroy() {
}

// Drop silently (no storage, no bounce).
type FilterActionDiscard struct {
}

func (e FilterActionDiscard) Destroy() {
}

// Reject with `reason` (Sieve `reject`, RFC 5429). Terminal like
// [`FilterAction::Discard`]. A single-recipient, non-null-sender txn is
// refused `550 5.7.1 <reason>` at end-of-DATA; a multi-recipient or
// null-sender txn drops the recipient silently (no DSN — backscatter-safe).
type FilterActionReject struct {
	Reason string
}

func (e FilterActionReject) Destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

// File the message into `mailbox` instead of the disposition default.
type FilterActionFileInto struct {
	Mailbox string
}

func (e FilterActionFileInto) Destroy() {
	FfiDestroyerString{}.Destroy(e.Mailbox)
}

// Forward to `address` (mechanics owned by `mail-forwarding.md`).
// `redirect` is the rule's copy mode — `false` keeps the local copy
// (`copy`, the default), `true` forwards with no local delivery
// (`redirect`): the MTA suppresses the recipient's local placement when
// any fired Forward says so (`mail-forwarding.md` § Per-rule "forward to").
type FilterActionForward struct {
	Address  string
	Redirect bool
}

func (e FilterActionForward) Destroy() {
	FfiDestroyerString{}.Destroy(e.Address)
	FfiDestroyerBool{}.Destroy(e.Redirect)
}

// Vacation auto-reply (Sieve `vacation`, RFC 5230). Non-placement and
// non-terminal — fires after delivery, subject to the [`auto_reply_decision`]
// loop guard and the `(recipient, sender)` rate limit (`interval_hours`).
type FilterActionAutoReply struct {
	Subject       string
	Body          string
	IntervalHours uint32
}

func (e FilterActionAutoReply) Destroy() {
	FfiDestroyerString{}.Destroy(e.Subject)
	FfiDestroyerString{}.Destroy(e.Body)
	FfiDestroyerUint32{}.Destroy(e.IntervalHours)
}

// Deliver normally but add the IMAP keyword/`label`.
type FilterActionAddLabel struct {
	Label string
}

func (e FilterActionAddLabel) Destroy() {
	FfiDestroyerString{}.Destroy(e.Label)
}

type FfiConverterFilterAction struct{}

var FfiConverterFilterActionINSTANCE = FfiConverterFilterAction{}

func (c FfiConverterFilterAction) Lift(rb RustBufferI) FilterAction {
	return LiftFromRustBuffer[FilterAction](c, rb)
}

func (c FfiConverterFilterAction) Lower(value FilterAction) C.RustBuffer {
	return LowerIntoRustBuffer[FilterAction](c, value)
}

func (c FfiConverterFilterAction) LowerExternal(value FilterAction) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[FilterAction](c, value))
}
func (FfiConverterFilterAction) Read(reader io.Reader) FilterAction {
	id := readInt32(reader)
	switch id {
	case 1:
		return FilterActionAllow{}
	case 2:
		return FilterActionDiscard{}
	case 3:
		return FilterActionReject{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 4:
		return FilterActionFileInto{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 5:
		return FilterActionForward{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterBoolINSTANCE.Read(reader),
		}
	case 6:
		return FilterActionAutoReply{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterUint32INSTANCE.Read(reader),
		}
	case 7:
		return FilterActionAddLabel{
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterFilterAction.Read()", id))
	}
}

func (FfiConverterFilterAction) Write(writer io.Writer, value FilterAction) {
	switch variant_value := value.(type) {
	case FilterActionAllow:
		writeInt32(writer, 1)
	case FilterActionDiscard:
		writeInt32(writer, 2)
	case FilterActionReject:
		writeInt32(writer, 3)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Reason)
	case FilterActionFileInto:
		writeInt32(writer, 4)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Mailbox)
	case FilterActionForward:
		writeInt32(writer, 5)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Address)
		FfiConverterBoolINSTANCE.Write(writer, variant_value.Redirect)
	case FilterActionAutoReply:
		writeInt32(writer, 6)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Subject)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Body)
		FfiConverterUint32INSTANCE.Write(writer, variant_value.IntervalHours)
	case FilterActionAddLabel:
		writeInt32(writer, 7)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Label)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterFilterAction.Write", value))
	}
}

type FfiDestroyerFilterAction struct{}

func (_ FfiDestroyerFilterAction) Destroy(value FilterAction) {
	value.Destroy()
}

// How a filter's conditions combine (`smtp-server.md` § Email filter rules,
// the wire `EmailFilter.combination` `"all"`/`"any"` string). Use
// [`FilterCombination::from_wire`] at the boundary; an unrecognized string maps
// to [`FilterCombination::All`], matching the legacy default.
type FilterCombination uint

const (
	// Every condition must match (`all`). Vacuously true for an empty rule set.
	FilterCombinationAll FilterCombination = 1
	// Any one condition matching is enough (`any`). False for an empty rule set.
	FilterCombinationAny FilterCombination = 2
)

type FfiConverterFilterCombination struct{}

var FfiConverterFilterCombinationINSTANCE = FfiConverterFilterCombination{}

func (c FfiConverterFilterCombination) Lift(rb RustBufferI) FilterCombination {
	return LiftFromRustBuffer[FilterCombination](c, rb)
}

func (c FfiConverterFilterCombination) Lower(value FilterCombination) C.RustBuffer {
	return LowerIntoRustBuffer[FilterCombination](c, value)
}

func (c FfiConverterFilterCombination) LowerExternal(value FilterCombination) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[FilterCombination](c, value))
}
func (FfiConverterFilterCombination) Read(reader io.Reader) FilterCombination {
	id := readInt32(reader)
	return FilterCombination(id)
}

func (FfiConverterFilterCombination) Write(writer io.Writer, value FilterCombination) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerFilterCombination struct{}

func (_ FfiDestroyerFilterCombination) Destroy(value FilterCombination) {
}

// A single match criterion. Mirrors `fauna_protocol::email::EmailFilterRule`
// plus [`FilterCondition::SpamScoreAtLeast`] (this track's addition).
type FilterCondition interface {
	Destroy()
}

// Envelope sender (`MAIL FROM`) equals `address`, case-insensitive.
type FilterConditionSenderIs struct {
	Address string
}

func (e FilterConditionSenderIs) Destroy() {
	FfiDestroyerString{}.Destroy(e.Address)
}

// The sender's domain (after the last `@`) equals `domain`, case-insensitive.
type FilterConditionSenderDomain struct {
	Domain string
}

func (e FilterConditionSenderDomain) Destroy() {
	FfiDestroyerString{}.Destroy(e.Domain)
}

// The `Subject` contains `text` (case-insensitive substring).
type FilterConditionSubjectContains struct {
	Text string
}

func (e FilterConditionSubjectContains) Destroy() {
	FfiDestroyerString{}.Destroy(e.Text)
}

// The decoded body ([`FilterContext::body`] — `text/plain` + `text/html`)
// contains `text` (case-insensitive substring). HTML matches against raw
// decoded markup (no tag-stripping) in v1.
type FilterConditionBodyContains struct {
	Text string
}

func (e FilterConditionBodyContains) Destroy() {
	FfiDestroyerString{}.Destroy(e.Text)
}

// A header named `name` is present (case-insensitive name match).
type FilterConditionHeaderExists struct {
	Name string
}

func (e FilterConditionHeaderExists) Destroy() {
	FfiDestroyerString{}.Destroy(e.Name)
}

// A header named `name` (case-insensitive) has a value containing `value`
// (case-insensitive substring).
type FilterConditionHeaderContains struct {
	Name  string
	Value string
}

func (e FilterConditionHeaderContains) Destroy() {
	FfiDestroyerString{}.Destroy(e.Name)
	FfiDestroyerString{}.Destroy(e.Value)
}

// The combined spam score (milli-int, per `crate::spam`) is `>= milli`.
// The "user-defined algorithm acts on the spam score" condition
// (`smtp-server.md:784`).
type FilterConditionSpamScoreAtLeast struct {
	Milli int32
}

func (e FilterConditionSpamScoreAtLeast) Destroy() {
	FfiDestroyerInt32{}.Destroy(e.Milli)
}

type FfiConverterFilterCondition struct{}

var FfiConverterFilterConditionINSTANCE = FfiConverterFilterCondition{}

func (c FfiConverterFilterCondition) Lift(rb RustBufferI) FilterCondition {
	return LiftFromRustBuffer[FilterCondition](c, rb)
}

func (c FfiConverterFilterCondition) Lower(value FilterCondition) C.RustBuffer {
	return LowerIntoRustBuffer[FilterCondition](c, value)
}

func (c FfiConverterFilterCondition) LowerExternal(value FilterCondition) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[FilterCondition](c, value))
}
func (FfiConverterFilterCondition) Read(reader io.Reader) FilterCondition {
	id := readInt32(reader)
	switch id {
	case 1:
		return FilterConditionSenderIs{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 2:
		return FilterConditionSenderDomain{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 3:
		return FilterConditionSubjectContains{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 4:
		return FilterConditionBodyContains{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 5:
		return FilterConditionHeaderExists{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 6:
		return FilterConditionHeaderContains{
			FfiConverterStringINSTANCE.Read(reader),
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 7:
		return FilterConditionSpamScoreAtLeast{
			FfiConverterInt32INSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterFilterCondition.Read()", id))
	}
}

func (FfiConverterFilterCondition) Write(writer io.Writer, value FilterCondition) {
	switch variant_value := value.(type) {
	case FilterConditionSenderIs:
		writeInt32(writer, 1)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Address)
	case FilterConditionSenderDomain:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Domain)
	case FilterConditionSubjectContains:
		writeInt32(writer, 3)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Text)
	case FilterConditionBodyContains:
		writeInt32(writer, 4)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Text)
	case FilterConditionHeaderExists:
		writeInt32(writer, 5)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Name)
	case FilterConditionHeaderContains:
		writeInt32(writer, 6)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Name)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Value)
	case FilterConditionSpamScoreAtLeast:
		writeInt32(writer, 7)
		FfiConverterInt32INSTANCE.Write(writer, variant_value.Milli)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterFilterCondition.Write", value))
	}
}

type FfiDestroyerFilterCondition struct{}

func (_ FfiDestroyerFilterCondition) Destroy(value FilterCondition) {
	value.Destroy()
}

type ICalError struct {
	err error
}

// Convenience method to turn *ICalError into error
// Avoiding treating nil pointer as non nil error interface
func (err *ICalError) AsError() error {
	if err == nil {
		return nil
	} else {
		return err
	}
}

func (err ICalError) Error() string {
	return fmt.Sprintf("ICalError: %s", err.err.Error())
}

func (err ICalError) Unwrap() error {
	return err.err
}

// Err* are used for checking error type with `errors.Is`
var ErrICalErrorMalformed = fmt.Errorf("ICalErrorMalformed")
var ErrICalErrorBadRrule = fmt.Errorf("ICalErrorBadRrule")
var ErrICalErrorBadDateTime = fmt.Errorf("ICalErrorBadDateTime")

// Variant structs
type ICalErrorMalformed struct {
	message string
}

func NewICalErrorMalformed() *ICalError {
	return &ICalError{err: &ICalErrorMalformed{}}
}

func (e ICalErrorMalformed) destroy() {
}

func (err ICalErrorMalformed) Error() string {
	return fmt.Sprintf("Malformed: %s", err.message)
}

func (self ICalErrorMalformed) Is(target error) bool {
	return target == ErrICalErrorMalformed
}

type ICalErrorBadRrule struct {
	message string
}

func NewICalErrorBadRrule() *ICalError {
	return &ICalError{err: &ICalErrorBadRrule{}}
}

func (e ICalErrorBadRrule) destroy() {
}

func (err ICalErrorBadRrule) Error() string {
	return fmt.Sprintf("BadRrule: %s", err.message)
}

func (self ICalErrorBadRrule) Is(target error) bool {
	return target == ErrICalErrorBadRrule
}

type ICalErrorBadDateTime struct {
	message string
}

func NewICalErrorBadDateTime() *ICalError {
	return &ICalError{err: &ICalErrorBadDateTime{}}
}

func (e ICalErrorBadDateTime) destroy() {
}

func (err ICalErrorBadDateTime) Error() string {
	return fmt.Sprintf("BadDateTime: %s", err.message)
}

func (self ICalErrorBadDateTime) Is(target error) bool {
	return target == ErrICalErrorBadDateTime
}

type FfiConverterICalError struct{}

var FfiConverterICalErrorINSTANCE = FfiConverterICalError{}

func (c FfiConverterICalError) Lift(eb RustBufferI) *ICalError {
	return LiftFromRustBuffer[*ICalError](c, eb)
}

func (c FfiConverterICalError) Lower(value *ICalError) C.RustBuffer {
	return LowerIntoRustBuffer[*ICalError](c, value)
}

func (c FfiConverterICalError) LowerExternal(value *ICalError) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*ICalError](c, value))
}

func (c FfiConverterICalError) Read(reader io.Reader) *ICalError {
	errorID := readUint32(reader)

	message := FfiConverterStringINSTANCE.Read(reader)
	switch errorID {
	case 1:
		return &ICalError{&ICalErrorMalformed{message}}
	case 2:
		return &ICalError{&ICalErrorBadRrule{message}}
	case 3:
		return &ICalError{&ICalErrorBadDateTime{message}}
	default:
		panic(fmt.Sprintf("Unknown error code %d in FfiConverterICalError.Read()", errorID))
	}

}

func (c FfiConverterICalError) Write(writer io.Writer, value *ICalError) {
	switch variantValue := value.err.(type) {
	case *ICalErrorMalformed:
		writeInt32(writer, 1)
	case *ICalErrorBadRrule:
		writeInt32(writer, 2)
	case *ICalErrorBadDateTime:
		writeInt32(writer, 3)
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiConverterICalError.Write", value))
	}
}

type FfiDestroyerICalError struct{}

func (_ FfiDestroyerICalError) Destroy(value *ICalError) {
	switch variantValue := value.err.(type) {
	case ICalErrorMalformed:
		variantValue.destroy()
	case ICalErrorBadRrule:
		variantValue.destroy()
	case ICalErrorBadDateTime:
		variantValue.destroy()
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiDestroyerICalError.Destroy", value))
	}
}

type ParseError struct {
	err error
}

// Convenience method to turn *ParseError into error
// Avoiding treating nil pointer as non nil error interface
func (err *ParseError) AsError() error {
	if err == nil {
		return nil
	} else {
		return err
	}
}

func (err ParseError) Error() string {
	return fmt.Sprintf("ParseError: %s", err.err.Error())
}

func (err ParseError) Unwrap() error {
	return err.err
}

// Err* are used for checking error type with `errors.Is`
var ErrParseErrorMalformed = fmt.Errorf("ParseErrorMalformed")
var ErrParseErrorUnsupported = fmt.Errorf("ParseErrorUnsupported")
var ErrParseErrorUnknownCte = fmt.Errorf("ParseErrorUnknownCte")

// Variant structs
type ParseErrorMalformed struct {
}

func NewParseErrorMalformed() *ParseError {
	return &ParseError{err: &ParseErrorMalformed{}}
}

func (e ParseErrorMalformed) destroy() {
}

func (err ParseErrorMalformed) Error() string {
	return fmt.Sprint("Malformed")
}

func (self ParseErrorMalformed) Is(target error) bool {
	return target == ErrParseErrorMalformed
}

// The message parsed fine, but the requested operation is not yet
// supported. Used by [`crate::bodysection::fetch_body_section`] for
// RFC 9051 §6.4.5 section forms outside the implemented slice
// (numbered-part addressing `BODY[N…]`, top-level `MIME`) — distinct
// from `Malformed` so the caller can tell "your message is broken"
// apart from "this section form isn't wired yet".
type ParseErrorUnsupported struct {
}

// The message parsed fine, but the requested operation is not yet
// supported. Used by [`crate::bodysection::fetch_body_section`] for
// RFC 9051 §6.4.5 section forms outside the implemented slice
// (numbered-part addressing `BODY[N…]`, top-level `MIME`) — distinct
// from `Malformed` so the caller can tell "your message is broken"
// apart from "this section form isn't wired yet".
func NewParseErrorUnsupported() *ParseError {
	return &ParseError{err: &ParseErrorUnsupported{}}
}

func (e ParseErrorUnsupported) destroy() {
}

func (err ParseErrorUnsupported) Error() string {
	return fmt.Sprint("Unsupported")
}

func (self ParseErrorUnsupported) Is(target error) bool {
	return target == ErrParseErrorUnsupported
}

// A `BINARY[…]` FETCH (RFC 3516 / RFC 9051 §6.4.5) addressed a part
// whose `Content-Transfer-Encoding` is one this server cannot decode
// (anything other than `7bit` / `8bit` / `binary` / `quoted-printable`
// / `base64`). Used by [`crate::bodysection::fetch_binary_section`];
// the MDA maps it to a tagged `NO [UNKNOWN-CTE]` (RFC 9051 §6.4.5 — the
// server "MUST fail the request" rather than return undecoded bytes).
type ParseErrorUnknownCte struct {
}

// A `BINARY[…]` FETCH (RFC 3516 / RFC 9051 §6.4.5) addressed a part
// whose `Content-Transfer-Encoding` is one this server cannot decode
// (anything other than `7bit` / `8bit` / `binary` / `quoted-printable`
// / `base64`). Used by [`crate::bodysection::fetch_binary_section`];
// the MDA maps it to a tagged `NO [UNKNOWN-CTE]` (RFC 9051 §6.4.5 — the
// server "MUST fail the request" rather than return undecoded bytes).
func NewParseErrorUnknownCte() *ParseError {
	return &ParseError{err: &ParseErrorUnknownCte{}}
}

func (e ParseErrorUnknownCte) destroy() {
}

func (err ParseErrorUnknownCte) Error() string {
	return fmt.Sprint("UnknownCte")
}

func (self ParseErrorUnknownCte) Is(target error) bool {
	return target == ErrParseErrorUnknownCte
}

type FfiConverterParseError struct{}

var FfiConverterParseErrorINSTANCE = FfiConverterParseError{}

func (c FfiConverterParseError) Lift(eb RustBufferI) *ParseError {
	return LiftFromRustBuffer[*ParseError](c, eb)
}

func (c FfiConverterParseError) Lower(value *ParseError) C.RustBuffer {
	return LowerIntoRustBuffer[*ParseError](c, value)
}

func (c FfiConverterParseError) LowerExternal(value *ParseError) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*ParseError](c, value))
}

func (c FfiConverterParseError) Read(reader io.Reader) *ParseError {
	errorID := readUint32(reader)

	switch errorID {
	case 1:
		return &ParseError{&ParseErrorMalformed{}}
	case 2:
		return &ParseError{&ParseErrorUnsupported{}}
	case 3:
		return &ParseError{&ParseErrorUnknownCte{}}
	default:
		panic(fmt.Sprintf("Unknown error code %d in FfiConverterParseError.Read()", errorID))
	}
}

func (c FfiConverterParseError) Write(writer io.Writer, value *ParseError) {
	switch variantValue := value.err.(type) {
	case *ParseErrorMalformed:
		writeInt32(writer, 1)
	case *ParseErrorUnsupported:
		writeInt32(writer, 2)
	case *ParseErrorUnknownCte:
		writeInt32(writer, 3)
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiConverterParseError.Write", value))
	}
}

type FfiDestroyerParseError struct{}

func (_ FfiDestroyerParseError) Destroy(value *ParseError) {
	switch variantValue := value.err.(type) {
	case ParseErrorMalformed:
		variantValue.destroy()
	case ParseErrorUnsupported:
		variantValue.destroy()
	case ParseErrorUnknownCte:
		variantValue.destroy()
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiDestroyerParseError.Destroy", value))
	}
}

// The delivery action the bridge must take, after the ClamAV verdict.
//
// rspamd's score does **not** gate delivery in T1.4 (it is stored + header-
// stamped only; the `max(rspamd, weighted_bayesian)` feed into disposition is
// T3.1, `mail-spam.md` § Combined-score formula). Only ClamAV gates here.
type ScanAction interface {
	Destroy()
}

// Stamp scan headers, seal, deliver.
type ScanActionDeliver struct {
}

func (e ScanActionDeliver) Destroy() {
}

// `554 5.7.1 Message contains malware: <signature>` at the perimeter.
type ScanActionRejectMalware struct {
	Signature string
}

func (e ScanActionRejectMalware) Destroy() {
	FfiDestroyerString{}.Destroy(e.Signature)
}

// Deliver to the recipient's Junk, with infected headers.
type ScanActionJunk struct {
	Signature string
}

func (e ScanActionJunk) Destroy() {
	FfiDestroyerString{}.Destroy(e.Signature)
}

// Deliver with infected headers, no routing override.
type ScanActionTag struct {
	Signature string
}

func (e ScanActionTag) Destroy() {
	FfiDestroyerString{}.Destroy(e.Signature)
}

// `451 4.7.0` tempfail — scanner errored. Never allow-without-scan.
type ScanActionTempfail struct {
	Reason string
}

func (e ScanActionTempfail) Destroy() {
	FfiDestroyerString{}.Destroy(e.Reason)
}

type FfiConverterScanAction struct{}

var FfiConverterScanActionINSTANCE = FfiConverterScanAction{}

func (c FfiConverterScanAction) Lift(rb RustBufferI) ScanAction {
	return LiftFromRustBuffer[ScanAction](c, rb)
}

func (c FfiConverterScanAction) Lower(value ScanAction) C.RustBuffer {
	return LowerIntoRustBuffer[ScanAction](c, value)
}

func (c FfiConverterScanAction) LowerExternal(value ScanAction) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ScanAction](c, value))
}
func (FfiConverterScanAction) Read(reader io.Reader) ScanAction {
	id := readInt32(reader)
	switch id {
	case 1:
		return ScanActionDeliver{}
	case 2:
		return ScanActionRejectMalware{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 3:
		return ScanActionJunk{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 4:
		return ScanActionTag{
			FfiConverterStringINSTANCE.Read(reader),
		}
	case 5:
		return ScanActionTempfail{
			FfiConverterStringINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterScanAction.Read()", id))
	}
}

func (FfiConverterScanAction) Write(writer io.Writer, value ScanAction) {
	switch variant_value := value.(type) {
	case ScanActionDeliver:
		writeInt32(writer, 1)
	case ScanActionRejectMalware:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Signature)
	case ScanActionJunk:
		writeInt32(writer, 3)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Signature)
	case ScanActionTag:
		writeInt32(writer, 4)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Signature)
	case ScanActionTempfail:
		writeInt32(writer, 5)
		FfiConverterStringINSTANCE.Write(writer, variant_value.Reason)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterScanAction.Write", value))
	}
}

type FfiDestroyerScanAction struct{}

func (_ FfiDestroyerScanAction) Destroy(value ScanAction) {
	value.Destroy()
}

// Error parsing an rspamd `/checkv2` response. The bridge maps this to a
// `451` tempfail (never allow-without-score).
type ScanError struct {
	err error
}

// Convenience method to turn *ScanError into error
// Avoiding treating nil pointer as non nil error interface
func (err *ScanError) AsError() error {
	if err == nil {
		return nil
	} else {
		return err
	}
}

func (err ScanError) Error() string {
	return fmt.Sprintf("ScanError: %s", err.err.Error())
}

func (err ScanError) Unwrap() error {
	return err.err
}

// Err* are used for checking error type with `errors.Is`
var ErrScanErrorInvalidJson = fmt.Errorf("ScanErrorInvalidJson")
var ErrScanErrorMissingField = fmt.Errorf("ScanErrorMissingField")

// Variant structs
type ScanErrorInvalidJson struct {
	message string
}

func NewScanErrorInvalidJson() *ScanError {
	return &ScanError{err: &ScanErrorInvalidJson{}}
}

func (e ScanErrorInvalidJson) destroy() {
}

func (err ScanErrorInvalidJson) Error() string {
	return fmt.Sprintf("InvalidJson: %s", err.message)
}

func (self ScanErrorInvalidJson) Is(target error) bool {
	return target == ErrScanErrorInvalidJson
}

type ScanErrorMissingField struct {
	message string
}

func NewScanErrorMissingField() *ScanError {
	return &ScanError{err: &ScanErrorMissingField{}}
}

func (e ScanErrorMissingField) destroy() {
}

func (err ScanErrorMissingField) Error() string {
	return fmt.Sprintf("MissingField: %s", err.message)
}

func (self ScanErrorMissingField) Is(target error) bool {
	return target == ErrScanErrorMissingField
}

type FfiConverterScanError struct{}

var FfiConverterScanErrorINSTANCE = FfiConverterScanError{}

func (c FfiConverterScanError) Lift(eb RustBufferI) *ScanError {
	return LiftFromRustBuffer[*ScanError](c, eb)
}

func (c FfiConverterScanError) Lower(value *ScanError) C.RustBuffer {
	return LowerIntoRustBuffer[*ScanError](c, value)
}

func (c FfiConverterScanError) LowerExternal(value *ScanError) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*ScanError](c, value))
}

func (c FfiConverterScanError) Read(reader io.Reader) *ScanError {
	errorID := readUint32(reader)

	message := FfiConverterStringINSTANCE.Read(reader)
	switch errorID {
	case 1:
		return &ScanError{&ScanErrorInvalidJson{message}}
	case 2:
		return &ScanError{&ScanErrorMissingField{message}}
	default:
		panic(fmt.Sprintf("Unknown error code %d in FfiConverterScanError.Read()", errorID))
	}

}

func (c FfiConverterScanError) Write(writer io.Writer, value *ScanError) {
	switch variantValue := value.err.(type) {
	case *ScanErrorInvalidJson:
		writeInt32(writer, 1)
	case *ScanErrorMissingField:
		writeInt32(writer, 2)
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiConverterScanError.Write", value))
	}
}

type FfiDestroyerScanError struct{}

func (_ FfiDestroyerScanError) Destroy(value *ScanError) {
	switch variantValue := value.err.(type) {
	case ScanErrorInvalidJson:
		variantValue.destroy()
	case ScanErrorMissingField:
		variantValue.destroy()
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiDestroyerScanError.Destroy", value))
	}
}

// Failure building a sealed scheduling delivery. UniFFI mirror in the
// `fauna_mail` namespace (the cross-namespace `fauna_mls` Go-binding footgun —
// same reason the iCalendar writer records live in `icalendar.rs`).
type SchedulingDeliveryError struct {
	err error
}

// Convenience method to turn *SchedulingDeliveryError into error
// Avoiding treating nil pointer as non nil error interface
func (err *SchedulingDeliveryError) AsError() error {
	if err == nil {
		return nil
	} else {
		return err
	}
}

func (err SchedulingDeliveryError) Error() string {
	return fmt.Sprintf("SchedulingDeliveryError: %s", err.err.Error())
}

func (err SchedulingDeliveryError) Unwrap() error {
	return err.err
}

// Err* are used for checking error type with `errors.Is`
var ErrSchedulingDeliveryErrorBadSenderActorId = fmt.Errorf("SchedulingDeliveryErrorBadSenderActorId")
var ErrSchedulingDeliveryErrorSeal = fmt.Errorf("SchedulingDeliveryErrorSeal")

// Variant structs
// `sender_actor_id` was not exactly 32 bytes.
type SchedulingDeliveryErrorBadSenderActorId struct {
	Field0 uint32
}

// `sender_actor_id` was not exactly 32 bytes.
func NewSchedulingDeliveryErrorBadSenderActorId(
	var0 uint32,
) *SchedulingDeliveryError {
	return &SchedulingDeliveryError{err: &SchedulingDeliveryErrorBadSenderActorId{
		Field0: var0}}
}

func (e SchedulingDeliveryErrorBadSenderActorId) destroy() {
	FfiDestroyerUint32{}.Destroy(e.Field0)
}

func (err SchedulingDeliveryErrorBadSenderActorId) Error() string {
	return fmt.Sprint("BadSenderActorId",
		": ",

		"Field0=",
		err.Field0,
	)
}

func (self SchedulingDeliveryErrorBadSenderActorId) Is(target error) bool {
	return target == ErrSchedulingDeliveryErrorBadSenderActorId
}

// The recipient key package was malformed, or the MLS sealing failed.
type SchedulingDeliveryErrorSeal struct {
	Field0 string
}

// The recipient key package was malformed, or the MLS sealing failed.
func NewSchedulingDeliveryErrorSeal(
	var0 string,
) *SchedulingDeliveryError {
	return &SchedulingDeliveryError{err: &SchedulingDeliveryErrorSeal{
		Field0: var0}}
}

func (e SchedulingDeliveryErrorSeal) destroy() {
	FfiDestroyerString{}.Destroy(e.Field0)
}

func (err SchedulingDeliveryErrorSeal) Error() string {
	return fmt.Sprint("Seal",
		": ",

		"Field0=",
		err.Field0,
	)
}

func (self SchedulingDeliveryErrorSeal) Is(target error) bool {
	return target == ErrSchedulingDeliveryErrorSeal
}

type FfiConverterSchedulingDeliveryError struct{}

var FfiConverterSchedulingDeliveryErrorINSTANCE = FfiConverterSchedulingDeliveryError{}

func (c FfiConverterSchedulingDeliveryError) Lift(eb RustBufferI) *SchedulingDeliveryError {
	return LiftFromRustBuffer[*SchedulingDeliveryError](c, eb)
}

func (c FfiConverterSchedulingDeliveryError) Lower(value *SchedulingDeliveryError) C.RustBuffer {
	return LowerIntoRustBuffer[*SchedulingDeliveryError](c, value)
}

func (c FfiConverterSchedulingDeliveryError) LowerExternal(value *SchedulingDeliveryError) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*SchedulingDeliveryError](c, value))
}

func (c FfiConverterSchedulingDeliveryError) Read(reader io.Reader) *SchedulingDeliveryError {
	errorID := readUint32(reader)

	switch errorID {
	case 1:
		return &SchedulingDeliveryError{&SchedulingDeliveryErrorBadSenderActorId{
			Field0: FfiConverterUint32INSTANCE.Read(reader),
		}}
	case 2:
		return &SchedulingDeliveryError{&SchedulingDeliveryErrorSeal{
			Field0: FfiConverterStringINSTANCE.Read(reader),
		}}
	default:
		panic(fmt.Sprintf("Unknown error code %d in FfiConverterSchedulingDeliveryError.Read()", errorID))
	}
}

func (c FfiConverterSchedulingDeliveryError) Write(writer io.Writer, value *SchedulingDeliveryError) {
	switch variantValue := value.err.(type) {
	case *SchedulingDeliveryErrorBadSenderActorId:
		writeInt32(writer, 1)
		FfiConverterUint32INSTANCE.Write(writer, variantValue.Field0)
	case *SchedulingDeliveryErrorSeal:
		writeInt32(writer, 2)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Field0)
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiConverterSchedulingDeliveryError.Write", value))
	}
}

type FfiDestroyerSchedulingDeliveryError struct{}

func (_ FfiDestroyerSchedulingDeliveryError) Destroy(value *SchedulingDeliveryError) {
	switch variantValue := value.err.(type) {
	case SchedulingDeliveryErrorBadSenderActorId:
		variantValue.destroy()
	case SchedulingDeliveryErrorSeal:
		variantValue.destroy()
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiDestroyerSchedulingDeliveryError.Destroy", value))
	}
}

type SpamDisposition uint

const (
	// Deliver to INBOX.
	SpamDispositionAccept SpamDisposition = 1
	// The combined score reached the Junk tier — file to the recipient's Junk.
	SpamDispositionAcceptToSpamFolder SpamDisposition = 2
	// Filed to the recipient's Junk by a rule, whatever the score: the
	// sender's own DMARC `p=quarantine` on a failing message, a ClamAV hit
	// under the `junk` action, or an unlisted-recipient penalty that reached
	// the reject tier (one recipient of a multi-recipient message cannot be
	// refused). Kept apart from `AcceptToSpamFolder` so the record says why.
	SpamDispositionPolicyJunk SpamDisposition = 3
	// 554 at the MTA; never ingested.
	SpamDispositionReject SpamDisposition = 4
)

type FfiConverterSpamDisposition struct{}

var FfiConverterSpamDispositionINSTANCE = FfiConverterSpamDisposition{}

func (c FfiConverterSpamDisposition) Lift(rb RustBufferI) SpamDisposition {
	return LiftFromRustBuffer[SpamDisposition](c, rb)
}

func (c FfiConverterSpamDisposition) Lower(value SpamDisposition) C.RustBuffer {
	return LowerIntoRustBuffer[SpamDisposition](c, value)
}

func (c FfiConverterSpamDisposition) LowerExternal(value SpamDisposition) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[SpamDisposition](c, value))
}
func (FfiConverterSpamDisposition) Read(reader io.Reader) SpamDisposition {
	id := readInt32(reader)
	return SpamDisposition(id)
}

func (FfiConverterSpamDisposition) Write(writer io.Writer, value SpamDisposition) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerSpamDisposition struct{}

func (_ FfiDestroyerSpamDisposition) Destroy(value SpamDisposition) {
}

// UniFFI-visible open failure. Deliberately one flat variant: every
// cause (wrong key, tamper, truncation, bad key length) is equally
// terminal for the carrying unit — the Go side logs the detail and
// fails the unit, it never branches on the cause.
type StagedEnvelopeFfiError struct {
	err error
}

// Convenience method to turn *StagedEnvelopeFfiError into error
// Avoiding treating nil pointer as non nil error interface
func (err *StagedEnvelopeFfiError) AsError() error {
	if err == nil {
		return nil
	} else {
		return err
	}
}

func (err StagedEnvelopeFfiError) Error() string {
	return fmt.Sprintf("StagedEnvelopeFfiError: %s", err.err.Error())
}

func (err StagedEnvelopeFfiError) Unwrap() error {
	return err.err
}

// Err* are used for checking error type with `errors.Is`
var ErrStagedEnvelopeFfiErrorOpen = fmt.Errorf("StagedEnvelopeFfiErrorOpen")

// Variant structs
type StagedEnvelopeFfiErrorOpen struct {
	Detail string
}

func NewStagedEnvelopeFfiErrorOpen(
	detail string,
) *StagedEnvelopeFfiError {
	return &StagedEnvelopeFfiError{err: &StagedEnvelopeFfiErrorOpen{
		Detail: detail}}
}

func (e StagedEnvelopeFfiErrorOpen) destroy() {
	FfiDestroyerString{}.Destroy(e.Detail)
}

func (err StagedEnvelopeFfiErrorOpen) Error() string {
	return fmt.Sprint("Open",
		": ",

		"Detail=",
		err.Detail,
	)
}

func (self StagedEnvelopeFfiErrorOpen) Is(target error) bool {
	return target == ErrStagedEnvelopeFfiErrorOpen
}

type FfiConverterStagedEnvelopeFfiError struct{}

var FfiConverterStagedEnvelopeFfiErrorINSTANCE = FfiConverterStagedEnvelopeFfiError{}

func (c FfiConverterStagedEnvelopeFfiError) Lift(eb RustBufferI) *StagedEnvelopeFfiError {
	return LiftFromRustBuffer[*StagedEnvelopeFfiError](c, eb)
}

func (c FfiConverterStagedEnvelopeFfiError) Lower(value *StagedEnvelopeFfiError) C.RustBuffer {
	return LowerIntoRustBuffer[*StagedEnvelopeFfiError](c, value)
}

func (c FfiConverterStagedEnvelopeFfiError) LowerExternal(value *StagedEnvelopeFfiError) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*StagedEnvelopeFfiError](c, value))
}

func (c FfiConverterStagedEnvelopeFfiError) Read(reader io.Reader) *StagedEnvelopeFfiError {
	errorID := readUint32(reader)

	switch errorID {
	case 1:
		return &StagedEnvelopeFfiError{&StagedEnvelopeFfiErrorOpen{
			Detail: FfiConverterStringINSTANCE.Read(reader),
		}}
	default:
		panic(fmt.Sprintf("Unknown error code %d in FfiConverterStagedEnvelopeFfiError.Read()", errorID))
	}
}

func (c FfiConverterStagedEnvelopeFfiError) Write(writer io.Writer, value *StagedEnvelopeFfiError) {
	switch variantValue := value.err.(type) {
	case *StagedEnvelopeFfiErrorOpen:
		writeInt32(writer, 1)
		FfiConverterStringINSTANCE.Write(writer, variantValue.Detail)
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiConverterStagedEnvelopeFfiError.Write", value))
	}
}

type FfiDestroyerStagedEnvelopeFfiError struct{}

func (_ FfiDestroyerStagedEnvelopeFfiError) Destroy(value *StagedEnvelopeFfiError) {
	switch variantValue := value.err.(type) {
	case StagedEnvelopeFfiErrorOpen:
		variantValue.destroy()
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiDestroyerStagedEnvelopeFfiError.Destroy", value))
	}
}

// UniFFI mirror of `fauna_core::ical::ITipMethod` (the iTIP scheduling method
// carried by an iMIP message's `METHOD` property), in the `fauna_mail`
// namespace so the Go MDA binding resolves (same footgun as the writer
// records above). The `From` impl is an exhaustive match, so a new
// `fauna_core::ical::ITipMethod` variant is a compile error here until mirrored.
type WriterITipMethod uint

const (
	// Organizer invites attendees or pushes an update to an existing event.
	WriterITipMethodRequest WriterITipMethod = 1
	// Attendee responds with their participation status (`PARTSTAT`).
	WriterITipMethodReply WriterITipMethod = 2
	// Organizer cancels the event.
	WriterITipMethodCancel WriterITipMethod = 3
)

type FfiConverterWriterITipMethod struct{}

var FfiConverterWriterITipMethodINSTANCE = FfiConverterWriterITipMethod{}

func (c FfiConverterWriterITipMethod) Lift(rb RustBufferI) WriterITipMethod {
	return LiftFromRustBuffer[WriterITipMethod](c, rb)
}

func (c FfiConverterWriterITipMethod) Lower(value WriterITipMethod) C.RustBuffer {
	return LowerIntoRustBuffer[WriterITipMethod](c, value)
}

func (c FfiConverterWriterITipMethod) LowerExternal(value WriterITipMethod) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[WriterITipMethod](c, value))
}
func (FfiConverterWriterITipMethod) Read(reader io.Reader) WriterITipMethod {
	id := readInt32(reader)
	return WriterITipMethod(id)
}

func (FfiConverterWriterITipMethod) Write(writer io.Writer, value WriterITipMethod) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerWriterITipMethod struct{}

func (_ FfiDestroyerWriterITipMethod) Destroy(value WriterITipMethod) {
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

type FfiConverterOptionalBodySectionPartial struct{}

var FfiConverterOptionalBodySectionPartialINSTANCE = FfiConverterOptionalBodySectionPartial{}

func (c FfiConverterOptionalBodySectionPartial) Lift(rb RustBufferI) *BodySectionPartial {
	return LiftFromRustBuffer[*BodySectionPartial](c, rb)
}

func (_ FfiConverterOptionalBodySectionPartial) Read(reader io.Reader) *BodySectionPartial {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterBodySectionPartialINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalBodySectionPartial) Lower(value *BodySectionPartial) C.RustBuffer {
	return LowerIntoRustBuffer[*BodySectionPartial](c, value)
}

func (c FfiConverterOptionalBodySectionPartial) LowerExternal(value *BodySectionPartial) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*BodySectionPartial](c, value))
}

func (_ FfiConverterOptionalBodySectionPartial) Write(writer io.Writer, value *BodySectionPartial) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterBodySectionPartialINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalBodySectionPartial struct{}

func (_ FfiDestroyerOptionalBodySectionPartial) Destroy(value *BodySectionPartial) {
	if value != nil {
		FfiDestroyerBodySectionPartial{}.Destroy(*value)
	}
}

type FfiConverterOptionalImipDispatch struct{}

var FfiConverterOptionalImipDispatchINSTANCE = FfiConverterOptionalImipDispatch{}

func (c FfiConverterOptionalImipDispatch) Lift(rb RustBufferI) *ImipDispatch {
	return LiftFromRustBuffer[*ImipDispatch](c, rb)
}

func (_ FfiConverterOptionalImipDispatch) Read(reader io.Reader) *ImipDispatch {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterImipDispatchINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalImipDispatch) Lower(value *ImipDispatch) C.RustBuffer {
	return LowerIntoRustBuffer[*ImipDispatch](c, value)
}

func (c FfiConverterOptionalImipDispatch) LowerExternal(value *ImipDispatch) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*ImipDispatch](c, value))
}

func (_ FfiConverterOptionalImipDispatch) Write(writer io.Writer, value *ImipDispatch) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterImipDispatchINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalImipDispatch struct{}

func (_ FfiDestroyerOptionalImipDispatch) Destroy(value *ImipDispatch) {
	if value != nil {
		FfiDestroyerImipDispatch{}.Destroy(*value)
	}
}

type FfiConverterOptionalInboundInvite struct{}

var FfiConverterOptionalInboundInviteINSTANCE = FfiConverterOptionalInboundInvite{}

func (c FfiConverterOptionalInboundInvite) Lift(rb RustBufferI) *InboundInvite {
	return LiftFromRustBuffer[*InboundInvite](c, rb)
}

func (_ FfiConverterOptionalInboundInvite) Read(reader io.Reader) *InboundInvite {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterInboundInviteINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalInboundInvite) Lower(value *InboundInvite) C.RustBuffer {
	return LowerIntoRustBuffer[*InboundInvite](c, value)
}

func (c FfiConverterOptionalInboundInvite) LowerExternal(value *InboundInvite) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*InboundInvite](c, value))
}

func (_ FfiConverterOptionalInboundInvite) Write(writer io.Writer, value *InboundInvite) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterInboundInviteINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalInboundInvite struct{}

func (_ FfiDestroyerOptionalInboundInvite) Destroy(value *InboundInvite) {
	if value != nil {
		FfiDestroyerInboundInvite{}.Destroy(*value)
	}
}

type FfiConverterOptionalKindMetadata struct{}

var FfiConverterOptionalKindMetadataINSTANCE = FfiConverterOptionalKindMetadata{}

func (c FfiConverterOptionalKindMetadata) Lift(rb RustBufferI) *KindMetadata {
	return LiftFromRustBuffer[*KindMetadata](c, rb)
}

func (_ FfiConverterOptionalKindMetadata) Read(reader io.Reader) *KindMetadata {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterKindMetadataINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalKindMetadata) Lower(value *KindMetadata) C.RustBuffer {
	return LowerIntoRustBuffer[*KindMetadata](c, value)
}

func (c FfiConverterOptionalKindMetadata) LowerExternal(value *KindMetadata) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*KindMetadata](c, value))
}

func (_ FfiConverterOptionalKindMetadata) Write(writer io.Writer, value *KindMetadata) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterKindMetadataINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalKindMetadata struct{}

func (_ FfiDestroyerOptionalKindMetadata) Destroy(value *KindMetadata) {
	if value != nil {
		FfiDestroyerKindMetadata{}.Destroy(*value)
	}
}

type FfiConverterSequenceUint32 struct{}

var FfiConverterSequenceUint32INSTANCE = FfiConverterSequenceUint32{}

func (c FfiConverterSequenceUint32) Lift(rb RustBufferI) []uint32 {
	return LiftFromRustBuffer[[]uint32](c, rb)
}

func (c FfiConverterSequenceUint32) Read(reader io.Reader) []uint32 {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]uint32, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterUint32INSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceUint32) Lower(value []uint32) C.RustBuffer {
	return LowerIntoRustBuffer[[]uint32](c, value)
}

func (c FfiConverterSequenceUint32) LowerExternal(value []uint32) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]uint32](c, value))
}

func (c FfiConverterSequenceUint32) Write(writer io.Writer, value []uint32) {
	if len(value) > math.MaxInt32 {
		panic("[]uint32 is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterUint32INSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceUint32 struct{}

func (FfiDestroyerSequenceUint32) Destroy(sequence []uint32) {
	for _, value := range sequence {
		FfiDestroyerUint32{}.Destroy(value)
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

type FfiConverterSequenceBodyStructure struct{}

var FfiConverterSequenceBodyStructureINSTANCE = FfiConverterSequenceBodyStructure{}

func (c FfiConverterSequenceBodyStructure) Lift(rb RustBufferI) []BodyStructure {
	return LiftFromRustBuffer[[]BodyStructure](c, rb)
}

func (c FfiConverterSequenceBodyStructure) Read(reader io.Reader) []BodyStructure {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]BodyStructure, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterBodyStructureINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceBodyStructure) Lower(value []BodyStructure) C.RustBuffer {
	return LowerIntoRustBuffer[[]BodyStructure](c, value)
}

func (c FfiConverterSequenceBodyStructure) LowerExternal(value []BodyStructure) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]BodyStructure](c, value))
}

func (c FfiConverterSequenceBodyStructure) Write(writer io.Writer, value []BodyStructure) {
	if len(value) > math.MaxInt32 {
		panic("[]BodyStructure is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterBodyStructureINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceBodyStructure struct{}

func (FfiDestroyerSequenceBodyStructure) Destroy(sequence []BodyStructure) {
	for _, value := range sequence {
		FfiDestroyerBodyStructure{}.Destroy(value)
	}
}

type FfiConverterSequenceEnvelopeAddress struct{}

var FfiConverterSequenceEnvelopeAddressINSTANCE = FfiConverterSequenceEnvelopeAddress{}

func (c FfiConverterSequenceEnvelopeAddress) Lift(rb RustBufferI) []EnvelopeAddress {
	return LiftFromRustBuffer[[]EnvelopeAddress](c, rb)
}

func (c FfiConverterSequenceEnvelopeAddress) Read(reader io.Reader) []EnvelopeAddress {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]EnvelopeAddress, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterEnvelopeAddressINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceEnvelopeAddress) Lower(value []EnvelopeAddress) C.RustBuffer {
	return LowerIntoRustBuffer[[]EnvelopeAddress](c, value)
}

func (c FfiConverterSequenceEnvelopeAddress) LowerExternal(value []EnvelopeAddress) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]EnvelopeAddress](c, value))
}

func (c FfiConverterSequenceEnvelopeAddress) Write(writer io.Writer, value []EnvelopeAddress) {
	if len(value) > math.MaxInt32 {
		panic("[]EnvelopeAddress is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterEnvelopeAddressINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceEnvelopeAddress struct{}

func (FfiDestroyerSequenceEnvelopeAddress) Destroy(sequence []EnvelopeAddress) {
	for _, value := range sequence {
		FfiDestroyerEnvelopeAddress{}.Destroy(value)
	}
}

type FfiConverterSequenceExpandedOccurrence struct{}

var FfiConverterSequenceExpandedOccurrenceINSTANCE = FfiConverterSequenceExpandedOccurrence{}

func (c FfiConverterSequenceExpandedOccurrence) Lift(rb RustBufferI) []ExpandedOccurrence {
	return LiftFromRustBuffer[[]ExpandedOccurrence](c, rb)
}

func (c FfiConverterSequenceExpandedOccurrence) Read(reader io.Reader) []ExpandedOccurrence {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ExpandedOccurrence, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterExpandedOccurrenceINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceExpandedOccurrence) Lower(value []ExpandedOccurrence) C.RustBuffer {
	return LowerIntoRustBuffer[[]ExpandedOccurrence](c, value)
}

func (c FfiConverterSequenceExpandedOccurrence) LowerExternal(value []ExpandedOccurrence) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ExpandedOccurrence](c, value))
}

func (c FfiConverterSequenceExpandedOccurrence) Write(writer io.Writer, value []ExpandedOccurrence) {
	if len(value) > math.MaxInt32 {
		panic("[]ExpandedOccurrence is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterExpandedOccurrenceINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceExpandedOccurrence struct{}

func (FfiDestroyerSequenceExpandedOccurrence) Destroy(sequence []ExpandedOccurrence) {
	for _, value := range sequence {
		FfiDestroyerExpandedOccurrence{}.Destroy(value)
	}
}

type FfiConverterSequenceFilterHeader struct{}

var FfiConverterSequenceFilterHeaderINSTANCE = FfiConverterSequenceFilterHeader{}

func (c FfiConverterSequenceFilterHeader) Lift(rb RustBufferI) []FilterHeader {
	return LiftFromRustBuffer[[]FilterHeader](c, rb)
}

func (c FfiConverterSequenceFilterHeader) Read(reader io.Reader) []FilterHeader {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]FilterHeader, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterFilterHeaderINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceFilterHeader) Lower(value []FilterHeader) C.RustBuffer {
	return LowerIntoRustBuffer[[]FilterHeader](c, value)
}

func (c FfiConverterSequenceFilterHeader) LowerExternal(value []FilterHeader) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]FilterHeader](c, value))
}

func (c FfiConverterSequenceFilterHeader) Write(writer io.Writer, value []FilterHeader) {
	if len(value) > math.MaxInt32 {
		panic("[]FilterHeader is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterFilterHeaderINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceFilterHeader struct{}

func (FfiDestroyerSequenceFilterHeader) Destroy(sequence []FilterHeader) {
	for _, value := range sequence {
		FfiDestroyerFilterHeader{}.Destroy(value)
	}
}

type FfiConverterSequenceFilterMatch struct{}

var FfiConverterSequenceFilterMatchINSTANCE = FfiConverterSequenceFilterMatch{}

func (c FfiConverterSequenceFilterMatch) Lift(rb RustBufferI) []FilterMatch {
	return LiftFromRustBuffer[[]FilterMatch](c, rb)
}

func (c FfiConverterSequenceFilterMatch) Read(reader io.Reader) []FilterMatch {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]FilterMatch, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterFilterMatchINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceFilterMatch) Lower(value []FilterMatch) C.RustBuffer {
	return LowerIntoRustBuffer[[]FilterMatch](c, value)
}

func (c FfiConverterSequenceFilterMatch) LowerExternal(value []FilterMatch) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]FilterMatch](c, value))
}

func (c FfiConverterSequenceFilterMatch) Write(writer io.Writer, value []FilterMatch) {
	if len(value) > math.MaxInt32 {
		panic("[]FilterMatch is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterFilterMatchINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceFilterMatch struct{}

func (FfiDestroyerSequenceFilterMatch) Destroy(sequence []FilterMatch) {
	for _, value := range sequence {
		FfiDestroyerFilterMatch{}.Destroy(value)
	}
}

type FfiConverterSequenceICalComponent struct{}

var FfiConverterSequenceICalComponentINSTANCE = FfiConverterSequenceICalComponent{}

func (c FfiConverterSequenceICalComponent) Lift(rb RustBufferI) []ICalComponent {
	return LiftFromRustBuffer[[]ICalComponent](c, rb)
}

func (c FfiConverterSequenceICalComponent) Read(reader io.Reader) []ICalComponent {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ICalComponent, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterICalComponentINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceICalComponent) Lower(value []ICalComponent) C.RustBuffer {
	return LowerIntoRustBuffer[[]ICalComponent](c, value)
}

func (c FfiConverterSequenceICalComponent) LowerExternal(value []ICalComponent) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ICalComponent](c, value))
}

func (c FfiConverterSequenceICalComponent) Write(writer io.Writer, value []ICalComponent) {
	if len(value) > math.MaxInt32 {
		panic("[]ICalComponent is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterICalComponentINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceICalComponent struct{}

func (FfiDestroyerSequenceICalComponent) Destroy(sequence []ICalComponent) {
	for _, value := range sequence {
		FfiDestroyerICalComponent{}.Destroy(value)
	}
}

type FfiConverterSequenceICalParameter struct{}

var FfiConverterSequenceICalParameterINSTANCE = FfiConverterSequenceICalParameter{}

func (c FfiConverterSequenceICalParameter) Lift(rb RustBufferI) []ICalParameter {
	return LiftFromRustBuffer[[]ICalParameter](c, rb)
}

func (c FfiConverterSequenceICalParameter) Read(reader io.Reader) []ICalParameter {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ICalParameter, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterICalParameterINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceICalParameter) Lower(value []ICalParameter) C.RustBuffer {
	return LowerIntoRustBuffer[[]ICalParameter](c, value)
}

func (c FfiConverterSequenceICalParameter) LowerExternal(value []ICalParameter) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ICalParameter](c, value))
}

func (c FfiConverterSequenceICalParameter) Write(writer io.Writer, value []ICalParameter) {
	if len(value) > math.MaxInt32 {
		panic("[]ICalParameter is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterICalParameterINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceICalParameter struct{}

func (FfiDestroyerSequenceICalParameter) Destroy(sequence []ICalParameter) {
	for _, value := range sequence {
		FfiDestroyerICalParameter{}.Destroy(value)
	}
}

type FfiConverterSequenceICalProperty struct{}

var FfiConverterSequenceICalPropertyINSTANCE = FfiConverterSequenceICalProperty{}

func (c FfiConverterSequenceICalProperty) Lift(rb RustBufferI) []ICalProperty {
	return LiftFromRustBuffer[[]ICalProperty](c, rb)
}

func (c FfiConverterSequenceICalProperty) Read(reader io.Reader) []ICalProperty {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ICalProperty, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterICalPropertyINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceICalProperty) Lower(value []ICalProperty) C.RustBuffer {
	return LowerIntoRustBuffer[[]ICalProperty](c, value)
}

func (c FfiConverterSequenceICalProperty) LowerExternal(value []ICalProperty) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ICalProperty](c, value))
}

func (c FfiConverterSequenceICalProperty) Write(writer io.Writer, value []ICalProperty) {
	if len(value) > math.MaxInt32 {
		panic("[]ICalProperty is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterICalPropertyINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceICalProperty struct{}

func (FfiDestroyerSequenceICalProperty) Destroy(sequence []ICalProperty) {
	for _, value := range sequence {
		FfiDestroyerICalProperty{}.Destroy(value)
	}
}

type FfiConverterSequenceMailBodyChunk struct{}

var FfiConverterSequenceMailBodyChunkINSTANCE = FfiConverterSequenceMailBodyChunk{}

func (c FfiConverterSequenceMailBodyChunk) Lift(rb RustBufferI) []MailBodyChunk {
	return LiftFromRustBuffer[[]MailBodyChunk](c, rb)
}

func (c FfiConverterSequenceMailBodyChunk) Read(reader io.Reader) []MailBodyChunk {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]MailBodyChunk, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterMailBodyChunkINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceMailBodyChunk) Lower(value []MailBodyChunk) C.RustBuffer {
	return LowerIntoRustBuffer[[]MailBodyChunk](c, value)
}

func (c FfiConverterSequenceMailBodyChunk) LowerExternal(value []MailBodyChunk) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]MailBodyChunk](c, value))
}

func (c FfiConverterSequenceMailBodyChunk) Write(writer io.Writer, value []MailBodyChunk) {
	if len(value) > math.MaxInt32 {
		panic("[]MailBodyChunk is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterMailBodyChunkINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceMailBodyChunk struct{}

func (FfiDestroyerSequenceMailBodyChunk) Destroy(sequence []MailBodyChunk) {
	for _, value := range sequence {
		FfiDestroyerMailBodyChunk{}.Destroy(value)
	}
}

type FfiConverterSequenceMimeParam struct{}

var FfiConverterSequenceMimeParamINSTANCE = FfiConverterSequenceMimeParam{}

func (c FfiConverterSequenceMimeParam) Lift(rb RustBufferI) []MimeParam {
	return LiftFromRustBuffer[[]MimeParam](c, rb)
}

func (c FfiConverterSequenceMimeParam) Read(reader io.Reader) []MimeParam {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]MimeParam, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterMimeParamINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceMimeParam) Lower(value []MimeParam) C.RustBuffer {
	return LowerIntoRustBuffer[[]MimeParam](c, value)
}

func (c FfiConverterSequenceMimeParam) LowerExternal(value []MimeParam) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]MimeParam](c, value))
}

func (c FfiConverterSequenceMimeParam) Write(writer io.Writer, value []MimeParam) {
	if len(value) > math.MaxInt32 {
		panic("[]MimeParam is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterMimeParamINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceMimeParam struct{}

func (FfiDestroyerSequenceMimeParam) Destroy(sequence []MimeParam) {
	for _, value := range sequence {
		FfiDestroyerMimeParam{}.Destroy(value)
	}
}

type FfiConverterSequenceParsedHeader struct{}

var FfiConverterSequenceParsedHeaderINSTANCE = FfiConverterSequenceParsedHeader{}

func (c FfiConverterSequenceParsedHeader) Lift(rb RustBufferI) []ParsedHeader {
	return LiftFromRustBuffer[[]ParsedHeader](c, rb)
}

func (c FfiConverterSequenceParsedHeader) Read(reader io.Reader) []ParsedHeader {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ParsedHeader, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterParsedHeaderINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceParsedHeader) Lower(value []ParsedHeader) C.RustBuffer {
	return LowerIntoRustBuffer[[]ParsedHeader](c, value)
}

func (c FfiConverterSequenceParsedHeader) LowerExternal(value []ParsedHeader) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ParsedHeader](c, value))
}

func (c FfiConverterSequenceParsedHeader) Write(writer io.Writer, value []ParsedHeader) {
	if len(value) > math.MaxInt32 {
		panic("[]ParsedHeader is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterParsedHeaderINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceParsedHeader struct{}

func (FfiDestroyerSequenceParsedHeader) Destroy(sequence []ParsedHeader) {
	for _, value := range sequence {
		FfiDestroyerParsedHeader{}.Destroy(value)
	}
}

type FfiConverterSequenceParsedMimePart struct{}

var FfiConverterSequenceParsedMimePartINSTANCE = FfiConverterSequenceParsedMimePart{}

func (c FfiConverterSequenceParsedMimePart) Lift(rb RustBufferI) []ParsedMimePart {
	return LiftFromRustBuffer[[]ParsedMimePart](c, rb)
}

func (c FfiConverterSequenceParsedMimePart) Read(reader io.Reader) []ParsedMimePart {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]ParsedMimePart, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterParsedMimePartINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceParsedMimePart) Lower(value []ParsedMimePart) C.RustBuffer {
	return LowerIntoRustBuffer[[]ParsedMimePart](c, value)
}

func (c FfiConverterSequenceParsedMimePart) LowerExternal(value []ParsedMimePart) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]ParsedMimePart](c, value))
}

func (c FfiConverterSequenceParsedMimePart) Write(writer io.Writer, value []ParsedMimePart) {
	if len(value) > math.MaxInt32 {
		panic("[]ParsedMimePart is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterParsedMimePartINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceParsedMimePart struct{}

func (FfiDestroyerSequenceParsedMimePart) Destroy(sequence []ParsedMimePart) {
	for _, value := range sequence {
		FfiDestroyerParsedMimePart{}.Destroy(value)
	}
}

type FfiConverterSequenceStoredFilter struct{}

var FfiConverterSequenceStoredFilterINSTANCE = FfiConverterSequenceStoredFilter{}

func (c FfiConverterSequenceStoredFilter) Lift(rb RustBufferI) []StoredFilter {
	return LiftFromRustBuffer[[]StoredFilter](c, rb)
}

func (c FfiConverterSequenceStoredFilter) Read(reader io.Reader) []StoredFilter {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]StoredFilter, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterStoredFilterINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceStoredFilter) Lower(value []StoredFilter) C.RustBuffer {
	return LowerIntoRustBuffer[[]StoredFilter](c, value)
}

func (c FfiConverterSequenceStoredFilter) LowerExternal(value []StoredFilter) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]StoredFilter](c, value))
}

func (c FfiConverterSequenceStoredFilter) Write(writer io.Writer, value []StoredFilter) {
	if len(value) > math.MaxInt32 {
		panic("[]StoredFilter is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterStoredFilterINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceStoredFilter struct{}

func (FfiDestroyerSequenceStoredFilter) Destroy(sequence []StoredFilter) {
	for _, value := range sequence {
		FfiDestroyerStoredFilter{}.Destroy(value)
	}
}

type FfiConverterSequenceWriterAttendeeInfo struct{}

var FfiConverterSequenceWriterAttendeeInfoINSTANCE = FfiConverterSequenceWriterAttendeeInfo{}

func (c FfiConverterSequenceWriterAttendeeInfo) Lift(rb RustBufferI) []WriterAttendeeInfo {
	return LiftFromRustBuffer[[]WriterAttendeeInfo](c, rb)
}

func (c FfiConverterSequenceWriterAttendeeInfo) Read(reader io.Reader) []WriterAttendeeInfo {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]WriterAttendeeInfo, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterWriterAttendeeInfoINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceWriterAttendeeInfo) Lower(value []WriterAttendeeInfo) C.RustBuffer {
	return LowerIntoRustBuffer[[]WriterAttendeeInfo](c, value)
}

func (c FfiConverterSequenceWriterAttendeeInfo) LowerExternal(value []WriterAttendeeInfo) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]WriterAttendeeInfo](c, value))
}

func (c FfiConverterSequenceWriterAttendeeInfo) Write(writer io.Writer, value []WriterAttendeeInfo) {
	if len(value) > math.MaxInt32 {
		panic("[]WriterAttendeeInfo is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterWriterAttendeeInfoINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceWriterAttendeeInfo struct{}

func (FfiDestroyerSequenceWriterAttendeeInfo) Destroy(sequence []WriterAttendeeInfo) {
	for _, value := range sequence {
		FfiDestroyerWriterAttendeeInfo{}.Destroy(value)
	}
}

type FfiConverterSequenceFilterCondition struct{}

var FfiConverterSequenceFilterConditionINSTANCE = FfiConverterSequenceFilterCondition{}

func (c FfiConverterSequenceFilterCondition) Lift(rb RustBufferI) []FilterCondition {
	return LiftFromRustBuffer[[]FilterCondition](c, rb)
}

func (c FfiConverterSequenceFilterCondition) Read(reader io.Reader) []FilterCondition {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]FilterCondition, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterFilterConditionINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceFilterCondition) Lower(value []FilterCondition) C.RustBuffer {
	return LowerIntoRustBuffer[[]FilterCondition](c, value)
}

func (c FfiConverterSequenceFilterCondition) LowerExternal(value []FilterCondition) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]FilterCondition](c, value))
}

func (c FfiConverterSequenceFilterCondition) Write(writer io.Writer, value []FilterCondition) {
	if len(value) > math.MaxInt32 {
		panic("[]FilterCondition is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterFilterConditionINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceFilterCondition struct{}

func (FfiDestroyerSequenceFilterCondition) Destroy(sequence []FilterCondition) {
	for _, value := range sequence {
		FfiDestroyerFilterCondition{}.Destroy(value)
	}
}

const (
	uniffiRustFuturePollReady      int8 = 0
	uniffiRustFuturePollMaybeReady int8 = 1
)

type rustFuturePollFunc func(C.uint64_t, C.UniffiRustFutureContinuationCallback, C.uint64_t)
type rustFutureCompleteFunc[T any] func(C.uint64_t, *C.RustCallStatus) T
type rustFutureFreeFunc func(C.uint64_t)

//export fauna_mail_uniffiFutureContinuationCallback
func fauna_mail_uniffiFutureContinuationCallback(data C.uint64_t, pollResult C.int8_t) {
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
			(C.UniffiRustFutureContinuationCallback)(C.fauna_mail_uniffiFutureContinuationCallback),
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

//export fauna_mail_uniffiFreeGorutine
func fauna_mail_uniffiFreeGorutine(data C.uint64_t) {
	handle := cgo.Handle(uintptr(data))
	defer handle.Delete()

	guard := handle.Value().(chan struct{})
	guard <- struct{}{}
}

// Verify SPF, DKIM, DMARC, and ARC for an inbound message.
//
// `raw` is the original RFC 5322 bytes (signature verification requires
// the canonical wire form, so we never accept a re-serialized
// `ParsedMessage` here). `mail_from` is the SMTP envelope sender address.
// `client_ip` is the connecting MTA's IP as a string (e.g. `"192.0.2.1"`).
// `client_helo` is the HELO/EHLO hostname. The DNS resolver is built from
// system configuration.
//
// Note: `client_ip` is accepted as `String` because `std::net::IpAddr` has
// no UniFFI binding. An unparseable IP falls back to `127.0.0.1`.
func VerifyInbound(raw []byte, mailFrom string, clientIp string, clientHelo string) (fauna_core.AuthVerdicts, error) {
	res, err := uniffiRustCallAsync[*AuthError](
		FfiConverterAuthErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_fauna_mail_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) fauna_core.AuthVerdicts {
			return fauna_core.FfiConverterAuthVerdictsINSTANCE.Lift(ffi)
		},
		C.uniffi_fauna_mail_fn_func_verify_inbound(FfiConverterBytesINSTANCE.Lower(raw), FfiConverterStringINSTANCE.Lower(mailFrom), FfiConverterStringINSTANCE.Lower(clientIp), FfiConverterStringINSTANCE.Lower(clientHelo)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_fauna_mail_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_fauna_mail_rust_future_free_rust_buffer(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}

// UniFFI: rejoin chunks fetched back off the byte plane (the Go MDA, on a
// `FETCH` whose reply carried a reference).
func JoinSealedMailBody(chunks [][]byte) []byte {
	return FfiConverterBytesINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_join_sealed_mail_body(FfiConverterSequenceBytesINSTANCE.Lower(chunks), _uniffiStatus),
		}
	}))
}

// UniFFI: does this sealed pair need to cross by reference?
func MailBodyNeedsReference(sealedBodyLen uint64, sealedHintLen uint64) bool {
	return FfiConverterBoolINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int8_t {
		return C.uniffi_fauna_mail_fn_func_mail_body_needs_reference(FfiConverterUint64INSTANCE.Lower(sealedBodyLen), FfiConverterUint64INSTANCE.Lower(sealedHintLen), _uniffiStatus)
	}))
}

// UniFFI: split a sealed body into content-addressed chunks (the Go MTA
// uploads each, then sends the hashes as the reference).
func SplitSealedMailBody(body []byte) []MailBodyChunk {
	return FfiConverterSequenceMailBodyChunkINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_split_sealed_mail_body(FfiConverterBytesINSTANCE.Lower(body), _uniffiStatus),
		}
	}))
}

// Extract the requested `BINARY[<section-binary>]` octets from raw RFC 5322
// bytes (RFC 3516 / RFC 9051 §6.4.5).
//
// Returns the addressed part's contents with its `Content-Transfer-Encoding`
// decoded (base64 / quoted-printable) — **no charset conversion**, unlike a
// `BODY[…]` text fetch through `mail-parser`'s `PartType::Text`. An empty
// `spec.part` returns the whole message (its header block + decoded body, like
// `BODY[]`); a `multipart` / `message/rfc822` part has no leaf CTE and so is
// returned verbatim. The `<partial>` substring, if any, is applied last.
//
// # Errors
//
// - [`ParseError::Malformed`] — the message can't be parsed, or a declared
// base64 / quoted-printable body fails to decode.
// - [`ParseError::UnknownCte`] — the part declares a `Content-Transfer-
// Encoding` this server can't decode (anything other than `7bit` / `8bit` /
// `binary` / `quoted-printable` / `base64`); the MDA maps it to a tagged
// `NO [UNKNOWN-CTE]` rather than returning undecoded bytes.
func FetchBinarySection(raw []byte, spec BinarySectionSpec) ([]byte, error) {
	_uniffiRV, _uniffiErr := rustCallWithError[*ParseError](FfiConverterParseError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_fetch_binary_section(FfiConverterBytesINSTANCE.Lower(raw), FfiConverterBinarySectionSpecINSTANCE.Lower(spec), _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue []byte
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterBytesINSTANCE.Lift(_uniffiRV), nil
	}
}

// `BINARY.SIZE[<section-binary>]` (RFC 3516 / RFC 9051 §6.4.5): the octet
// count of the CTE-decoded section [`fetch_binary_section`] would return for
// the same `part` (no `<partial>` — `BINARY.SIZE` carries none).
//
// Errors are the same as [`fetch_binary_section`].
func FetchBinarySize(raw []byte, part []uint32) (uint32, error) {
	_uniffiRV, _uniffiErr := rustCallWithError[*ParseError](FfiConverterParseError{}, func(_uniffiStatus *C.RustCallStatus) C.uint32_t {
		return C.uniffi_fauna_mail_fn_func_fetch_binary_size(FfiConverterBytesINSTANCE.Lower(raw), FfiConverterSequenceUint32INSTANCE.Lower(part), _uniffiStatus)
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue uint32
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterUint32INSTANCE.Lift(_uniffiRV), nil
	}
}

// Extract the requested `BODY[<section>]` octets from raw RFC 5322 bytes.
//
// Returns the verbatim section bytes (with the `<partial>` substring applied
// if present), `ParseError::Malformed` if the message can't be parsed, or
// `ParseError::Unsupported` for the top-level `MIME` specifier (which only has
// meaning for a numbered part) or an unknown specifier.
func FetchBodySection(raw []byte, spec BodySectionSpec) ([]byte, error) {
	_uniffiRV, _uniffiErr := rustCallWithError[*ParseError](FfiConverterParseError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_fetch_body_section(FfiConverterBytesINSTANCE.Lower(raw), FfiConverterBodySectionSpecINSTANCE.Lower(spec), _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue []byte
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterBytesINSTANCE.Lift(_uniffiRV), nil
	}
}

// Derive an IMAP BODYSTRUCTURE tree from raw RFC 5322 / MIME bytes.
//
// The bridge layer (Go) consumes this to emit `BODYSTRUCTURE` /
// `BODY` FETCH responses without re-parsing on the Go side.
func DeriveBodyStructure(raw []byte) (BodyStructure, error) {
	_uniffiRV, _uniffiErr := rustCallWithError[*ParseError](FfiConverterParseError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_derive_body_structure(FfiConverterBytesINSTANCE.Lower(raw), _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue BodyStructure
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterBodyStructureINSTANCE.Lift(_uniffiRV), nil
	}
}

// The canonical dedup keys for one raw RFC 5322 message.
//
// Both are printable ASCII strings for the `actor_message_dedup` `TEXT`
// columns. An unparseable message still yields stable keys (the envelope
// form over empty headers plus the body hash) rather than failing — the
// caller is mid-delivery and must not drop mail over a dedup detail.
//
// Takes `Vec<u8>` because UniFFI cannot export a borrowed slice, and the Go
// MDA/MTA reach this over that binding. In-process Rust callers — the import
// client, which already holds the body it is about to send — should call
// [`mail_dedup_keys_from_slice`] and skip a copy of up to 50 MiB per message.
func MailDedupKeys(rawMessage []byte) MailDedupKeyPair {
	return FfiConverterMailDedupKeyPairINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_mail_dedup_keys(FfiConverterBytesINSTANCE.Lower(rawMessage), _uniffiStatus),
		}
	}))
}

// Derive an IMAP ENVELOPE from raw RFC 5322 bytes.
//
// Re-parses internally via `mail-parser`; callers supply the wire bytes
// (post-decryption in the encrypted-mode path).
func DeriveEnvelope(raw []byte) (Envelope, error) {
	_uniffiRV, _uniffiErr := rustCallWithError[*ParseError](FfiConverterParseError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_derive_envelope(FfiConverterBytesINSTANCE.Lower(raw), _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue Envelope
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterEnvelopeINSTANCE.Lift(_uniffiRV), nil
	}
}

// Every mailbox the message's `From:` field names that carries both a local
// part and a domain — the addresses a door that checks *who the message
// claims to be from* has to look at, in header order, headers only
// (`parse_headers`, like [`sender_domain`]).
//
// The filter is [`sender_domain`]'s, so the first entry's `host` (lowercased)
// IS the DKIM `d=` anchor that function returns: a door that refuses on this
// list and a signer that keys on that anchor can never disagree about which
// address the message is from. Display-only or domain-less entries are
// dropped for the same reason — they anchor nothing and own nothing. Group
// syntax is flattened, so a `From:` group with two members counts two.
//
// The submission door (465/587) refuses a count other than one and then
// checks that the one address, when the deployment signs for its domain, is
// owned by the authenticated actor (`mail-multidomain.md` § From: header
// ownership). `mailbox` keeps the header's case; the door compares local
// parts case-insensitively.
func FromMailboxes(raw []byte) []EnvelopeAddress {
	return FfiConverterSequenceEnvelopeAddressINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_from_mailboxes(FfiConverterBytesINSTANCE.Lower(raw), _uniffiStatus),
		}
	}))
}

// [`sender_domain`] plus the SMTP-envelope fallback the Go MTA/MDA callers
// need: when the `From:` header yields no domain (absent, unparseable, or no
// address carrying both a local part and a domain), fall back to the domain
// of `mail_from` — the SMTP `MAIL FROM:<…>` reverse-path addr-spec. Callers
// with no envelope (IMAP APPEND, the DKIM `d=` anchor) pass `mail_from = ""`.
//
// This is the ONE implementation of the `from_norm` rule (IMAP `SEARCH
// FROM`, the SPF-audit path); it replaced the Go
// `mailfauna.ExtractSenderDomain` copy (lifted 2026-07-12, priority #2).
// Two deliberate semantic deltas from that copy, pinned by the golden corpus
// (`the_golden_corpus_pins_the_lifted_from_norm_rule` here, mirrored by the
// Go `TestSenderDomainGoldenCorpus` over the FFI):
//
// 1. A **multi-address or group** `From:` yields the FIRST address's domain.
// The Go copy's strict `net/mail.ParseAddress` errored on those and fell
// through to the envelope (or `""`), so `SEARCH FROM` / the SPF audit got
// the bounce address's domain — or nothing — for real-world mail.
// 2. Header parsing is `mail-parser`'s **lenient** real-world grammar — the
// same parser that produced the `parse_rfc5322` result the callers hold —
// rather than strict RFC 5322, so `from_norm` can no longer disagree with
// the rest of the parse about the same bytes.
func SenderDomainWithEnvelopeFallback(raw []byte, mailFrom string) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_sender_domain_with_envelope_fallback(FfiConverterBytesINSTANCE.Lower(raw), FfiConverterStringINSTANCE.Lower(mailFrom), _uniffiStatus),
		}
	}))
}

// Decide whether a matched `AutoReply` action should fire, applying the RFC 5230
// §4.4/§4.6 + RFC 3834 loop guards in order. Pure + perimeter-evaluable: it
// reads only the envelope sender, the message headers, the delivery recipient
// address, and our hosted `local_domains` — all available at the plaintext floor
// pre-seal in both storage modes. The stateful rate-limit (`auto_reply_log`)
// is a separate check the caller makes only when this returns
// [`AutoReplyGate::Send`].
func AutoReplyDecision(envelopeFrom string, headers []FilterHeader, recipientAddr string, localDomains []string) AutoReplyGate {
	return FfiConverterAutoReplyGateINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_auto_reply_decision(FfiConverterStringINSTANCE.Lower(envelopeFrom), FfiConverterSequenceFilterHeaderINSTANCE.Lower(headers), FfiConverterStringINSTANCE.Lower(recipientAddr), FfiConverterSequenceStringINSTANCE.Lower(localDomains), _uniffiStatus),
		}
	}))
}

// Evaluate `filters` against `ctx`, returning the **ordered** actions to apply
// (empty if nothing matched — the caller falls through to spam-disposition
// placement).
//
// **Order + `continue` (`smtp-server.md:861` — "Rules are evaluated in order.
// The first matching rule wins unless the rule is marked `continue`").**
// Self-contained + deterministic: the engine sorts by `priority ASC, id ASC`
// (the same order the DB's `list_email_filters` returns), so the caller need
// not pre-sort. It walks the rules in order, recording each match; a matched
// rule with [`StoredFilter::continue_on_match`] `== false` is **terminal** and
// stops the walk (the classic first-match-wins), while a `continue` match falls
// through to later rules. The returned `Vec` is therefore the matched rules'
// actions in evaluation order, ending at the first non-`continue` match (or the
// last rule). Action *composition* (placement override, label accumulation,
// `Discard` short-circuit) is the caller's job — `smtp-server.md` § Email filter
// rules spells out the precedence.
func Evaluate(filters []StoredFilter, ctx FilterContext) []FilterMatch {
	return FfiConverterSequenceFilterMatchINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_evaluate(FfiConverterSequenceStoredFilterINSTANCE.Lower(filters), FfiConverterFilterContextINSTANCE.Lower(ctx), _uniffiStatus),
		}
	}))
}

// How many `From:` header fields `raw`'s header section carries.
//
// The inbound SMTP DATA stage, SMTP submission and `fauna.email.send` accept a
// message only when this is exactly one (`smtp-server.md` § Architectural
// rules). Never fails: an empty or header-less input counts zero.
func FromFieldCount(raw []byte) uint32 {
	return FfiConverterUint32INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint32_t {
		return C.uniffi_fauna_mail_fn_func_from_field_count(FfiConverterBytesINSTANCE.Lower(raw), _uniffiStatus)
	}))
}

// Build the iMIP `REPLY` a calendar app's attendee owes the organizer after
// answering an invitation by re-storing the event with their `PARTSTAT` changed
// — the UniFFI entry point for the Go MDA's "Responding" half of
// caldav-server.md § Server-side auto-schedule. Thin wrapper over the
// single-sourced `fauna_core::ical::build_attendee_reply_imip` (priority #2):
// `None` when nothing is owed (no organizer, the attendee is the organizer, not
// rostered, no answer, or the answer is unchanged from `prior_ics`).
func BuildAttendeeReplyFromIcs(newIcs string, priorIcs *string, attendeeEmail string, dtstamp string) *ImipDispatch {
	return FfiConverterOptionalImipDispatchINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_build_attendee_reply_from_ics(FfiConverterStringINSTANCE.Lower(newIcs), FfiConverterOptionalStringINSTANCE.Lower(priorIcs), FfiConverterStringINSTANCE.Lower(attendeeEmail), FfiConverterStringINSTANCE.Lower(dtstamp), _uniffiStatus),
		}
	}))
}

// Build an iMIP scheduling email from a raw iCalendar event body — the UniFFI
// entry point for the Go MDA's server-side auto-schedule gateway
// (caldav-server.md § Server-side auto-schedule). Parses the event with the
// single-sourced `fauna_core::ical` reader, then builds the `METHOD`-tagged
// iMIP message via `fauna_core::ical::build_event_imip` — the *same* impl the
// native apps use (priority #2), so a server-fanned invite is byte-identical
// to a client-fanned one. Returns `None` when the body has no `ORGANIZER` or no
// email-reachable recipient (the gateway then skips the send). `dtstamp` is an
// RFC 3339 construction timestamp (kept a parameter so the writer stays pure +
// deterministic — the Go MDA supplies the PUT time).
func BuildEventImipFromIcs(method WriterITipMethod, rawIcs string, dtstamp string) *ImipDispatch {
	return FfiConverterOptionalImipDispatchINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_build_event_imip_from_ics(FfiConverterWriterITipMethodINSTANCE.Lower(method), FfiConverterStringINSTANCE.Lower(rawIcs), FfiConverterStringINSTANCE.Lower(dtstamp), _uniffiStatus),
		}
	}))
}

// Expand a component's RRULE inside `[window_start, window_end)`.
//
// `window_start` and `window_end` are unix epoch seconds. The caller is
// responsible for passing a component that has a DTSTART; if the component
// has no RRULE the function returns the single base occurrence when its
// DTSTART falls inside the window.
//
// VTIMEZONE-relative DTSTART is honored when the surrounding
// [`ICalDocument`]'s VTIMEZONE block is reachable — the caller must
// flatten any VTIMEZONE-anchored component to its UTC instant before
// calling, or pass a serialized VCALENDAR string via [`parse_icalendar`] +
// [`expand_recurrence`] in sequence (the public surface accepts a
// component, not a full calendar; VTIMEZONE-aware expansion is a follow-up
// once a real CalDAV MUA needs it. For the v1 MDA the time-range filter
// runs against MUA-PUT bodies whose DTSTART is normalized to UTC by the
// MUA — every modern CalDAV client does this, including Apple Calendar,
// Thunderbird, and Evolution).
func ExpandRecurrence(component ICalComponent, windowStart int64, windowEnd int64) ([]ExpandedOccurrence, error) {
	_uniffiRV, _uniffiErr := rustCallWithError[*ICalError](FfiConverterICalError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_expand_recurrence(FfiConverterICalComponentINSTANCE.Lower(component), FfiConverterInt64INSTANCE.Lower(windowStart), FfiConverterInt64INSTANCE.Lower(windowEnd), _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue []ExpandedOccurrence
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterSequenceExpandedOccurrenceINSTANCE.Lift(_uniffiRV), nil
	}
}

// Serialize an event to an RFC 5545 VCALENDAR string — the UniFFI entry point
// for the Go MDA auto-schedule gateway. Thin wrapper over the single-sourced
// `fauna_core::ical::generate_ical`; the Go MDA reaches it as
// `mailfauna.GenerateIcal` (sibling of `mailfauna.ParseICalendar`).
func GenerateIcal(event WriterEventFields, attendees []WriterAttendeeInfo, organizerEmail string) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_generate_ical(FfiConverterWriterEventFieldsINSTANCE.Lower(event), FfiConverterSequenceWriterAttendeeInfoINSTANCE.Lower(attendees), FfiConverterStringINSTANCE.Lower(organizerEmail), _uniffiStatus),
		}
	}))
}

// Build an iTIP/iMIP scheduling message (`METHOD`-tagged VCALENDAR) — the
// UniFFI entry point for the Go MDA's server-side auto-schedule gateway
// (caldav-server.md § Server-side auto-schedule). Thin wrapper over the
// single-sourced `fauna_core::ical::generate_itip` (the VEVENT writer is never
// duplicated); the Go MDA reaches it as `mailfauna.GenerateItip`, the sibling
// of `mailfauna.GenerateIcal`. `dtstamp` is an RFC 3339 timestamp (kept a
// parameter so the writer stays pure/deterministic — caller supplies the
// construction time).
func GenerateItip(method WriterITipMethod, event WriterEventFields, attendees []WriterAttendeeInfo, organizerEmail string, dtstamp string) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_generate_itip(FfiConverterWriterITipMethodINSTANCE.Lower(method), FfiConverterWriterEventFieldsINSTANCE.Lower(event), FfiConverterSequenceWriterAttendeeInfoINSTANCE.Lower(attendees), FfiConverterStringINSTANCE.Lower(organizerEmail), FfiConverterStringINSTANCE.Lower(dtstamp), _uniffiStatus),
		}
	}))
}

// Read an emailed invitation out of a raw RFC 5322 message — the inbound half
// of caldav-server.md § Server-side auto-schedule ("an external organizer
// invites a Fauna user … drops it on their calendar"). `Some` only for an iMIP
// `REQUEST` (RFC 6047) whose event carries a `UID`, a `DTSTART` and an
// `ORGANIZER`; every other message — ordinary mail, a `REPLY`, a `CANCEL`, a
// broken calendar part — is `None`, so a delivery path can call this once per
// message. The stored body is re-rendered through the shared writer
// (`fauna_core::ical::render_stored_event`, the renderer a Fauna app's own PUT
// uses) rather than stored as the sender wrote it: a stranger's bytes never
// reach a calendar collection unparsed, so a malformed invitation cannot break
// the collection for a calendar app. `timestamp` stamps the stored `DTSTAMP`.
// Exported for the Go MTA (external senders) and called by the nest (senders on
// its own domain) — one reading of an invitation for both delivery paths.
func InviteFromMail(rawRfc5322 []byte, timestamp int64) *InboundInvite {
	return FfiConverterOptionalInboundInviteINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_invite_from_mail(FfiConverterBytesINSTANCE.Lower(rawRfc5322), FfiConverterInt64INSTANCE.Lower(timestamp), _uniffiStatus),
		}
	}))
}

// Extract the `ORGANIZER` CAL-ADDRESS (bare email, `mailto:` stripped
// case-insensitively) from a raw iCalendar event body, or `None` when it has
// no `ORGANIZER`. The UniFFI entry point for the Go MDA's auto-schedule
// gateway (caldav-server.md § Server-side auto-schedule). Thin wrapper over the
// single-sourced `fauna_core::ical::parse_ical_organizer` (priority #2 — the
// `mailto:`/CAL-ADDRESS parse is never re-implemented in Go); the MDA reaches
// it as `mailfauna.ParseIcalOrganizer`, the sibling of `BuildEventImipFromICS`.
// It is the cheap organizer gate the gateway runs on a PUT *before* the
// (expensive) read-before-write roster diff: only the event's own organizer
// fans out, and an event whose new roster is empty (all attendees removed)
// still needs the organizer known to decide whether a `CANCEL` is owed.
func ParseIcalOrganizerFromIcs(rawIcs string) *string {
	return FfiConverterOptionalStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_parse_ical_organizer_from_ics(FfiConverterStringINSTANCE.Lower(rawIcs), _uniffiStatus),
		}
	}))
}

// Parse raw VCALENDAR bytes into a low-level component tree.
//
// Performs no semantic validation beyond structural correctness — the
// caller (e.g. the CalDAV PUT handler) enforces RFC 5545 invariants
// (`UID` / `DTSTAMP` / `DTSTART` present on every VEVENT).
func ParseIcalendar(bytes []byte) (ICalDocument, error) {
	_uniffiRV, _uniffiErr := rustCallWithError[*ICalError](FfiConverterICalError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_parse_icalendar(FfiConverterBytesINSTANCE.Lower(bytes), _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue ICalDocument
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterICalDocumentINSTANCE.Lift(_uniffiRV), nil
	}
}

// Look up a kind's metadata in the production protocol registry.
// Returns `None` if the kind is not registered.
func LookupKind(kind string) *KindMetadata {
	return FfiConverterOptionalKindMetadataINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_lookup_kind(FfiConverterStringINSTANCE.Lower(kind), _uniffiStatus),
		}
	}))
}

// A fresh Fauna-minted Message-ID local part — RNG included, for callers
// without their own (the Go submission server stamps
// `Message-ID: <{this}@{domain}>` when the submitting MUA omitted one).
// Native-only (`msgid-mint`); the wasm compose path supplies its own
// randomness to [`mint_local`] instead.
func NewFaunaMsgidLocal() string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_new_fauna_msgid_local(_uniffiStatus),
		}
	}))
}

// Compose the RFC 5322 wire bytes for a vacation auto-reply. `Auto-Submitted:
// auto-replied` marks it as automated so a conforming peer won't auto-reply back
// (RFC 3834), closing the reply-to-a-reply loop.
func ComposeAutoReply(msg AutoReplyMessage) []byte {
	return FfiConverterBytesINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_compose_auto_reply(FfiConverterAutoReplyMessageINSTANCE.Lower(msg), _uniffiStatus),
		}
	}))
}

func ParseRfc5322(raw []byte) (ParsedMessage, error) {
	_uniffiRV, _uniffiErr := rustCallWithError[*ParseError](FfiConverterParseError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_parse_rfc5322(FfiConverterBytesINSTANCE.Lower(raw), _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue ParsedMessage
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterParsedMessageINSTANCE.Lift(_uniffiRV), nil
	}
}

// Build one CRLF-folded canonical `Received:` header field, ready to hand to the
// Go `prependHeaders` as a single element (internal continuation lines are
// tab-indented per RFC 5322 §2.2.3; **no** trailing CRLF — the caller adds the
// one terminator). The final clause before the date carries the RFC 5321 §4.4
// `;`. Returns a `String` (ASCII).
func BuildReceivedHeader(nowUnixSecs int64, opts ReceivedHeaderOpts) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_build_received_header(FfiConverterInt64INSTANCE.Lower(nowUnixSecs), FfiConverterReceivedHeaderOptsINSTANCE.Lower(opts), _uniffiStatus),
		}
	}))
}

// The canonical 32-byte report-hash over a message's subject + body text —
// the same parsed fields the index tokenizer consumes
// (`ParsedMessage.Subject`/`.BodyText`).
func ReportHash(subject string, bodyText string) []byte {
	return FfiConverterBytesINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_report_hash(FfiConverterStringINSTANCE.Lower(subject), FfiConverterStringINSTANCE.Lower(bodyText), _uniffiStatus),
		}
	}))
}

// Parse a clamd `zINSTREAM` reply into a verdict.
//
// clamd replies (null-terminated): `stream: OK`, `stream: <sig> FOUND`, or an
// `... ERROR` line. Anything unrecognized is treated as an `Error` (never
// silently `Clean` — that was the legacy `clamd.rs` fail-open bug).
func ClamdParseReply(reply string) fauna_core.ClamavVerdict {
	return fauna_core.FfiConverterClamavVerdictINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_clamd_parse_reply(FfiConverterStringINSTANCE.Lower(reply), _uniffiStatus),
		}
	}))
}

// Decide the delivery action from the ClamAV verdict + policy.
//
// Centralizes the never-allow-without-scan rule: an `Error` verdict becomes a
// `Tempfail` (the bridge 451s and the sender's MTA retries). `Clean` and
// `BypassedOversize` deliver. Only ClamAV gates delivery in T1.4; rspamd's
// score is stored/header-stamped separately.
func DecideScanAction(clamav fauna_core.ClamavVerdict, policy ScanPolicy) ScanAction {
	return FfiConverterScanActionINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_decide_scan_action(
				CFromRustBuffer(fauna_core.FfiConverterClamavVerdictINSTANCE.LowerExternal(clamav)), FfiConverterScanPolicyINSTANCE.Lower(policy), _uniffiStatus),
		}
	}))
}

// Parse an rspamd `/checkv2` JSON response, applying `scaling_per_mille`.
//
// All scores are returned as milli-ints. `symbols` is optional (a clean
// message with no fired rules has none); a missing top-level `score` is an
// error (a malformed 200 → tempfail, never allow-without-score).
func RspamdParseReply(json string, scalingPerMille uint16) (fauna_core.RspamdScore, error) {
	_uniffiRV, _uniffiErr := rustCallWithError[*ScanError](FfiConverterScanError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_rspamd_parse_reply(FfiConverterStringINSTANCE.Lower(json), FfiConverterUint16INSTANCE.Lower(scalingPerMille), _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue fauna_core.RspamdScore
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return fauna_core.FfiConverterRspamdScoreINSTANCE.Lift(_uniffiRV), nil
	}
}

// Seal a one-off MLS scheduling delivery of `imip_rfc5322` to the recipient
// whose **consumed** key package is `recipient_kp` (fetched via
// `keypackage.fetch`), stamping `sender_actor_id` (the real organizer's 32-byte
// actor id) as the app-level sender. The MLS signing identity is a fresh
// ephemeral keypair (see the module docs). The Go MDA reaches it as
// `mailfauna.BuildSchedulingDelivery` and then delivers the result over the
// caller-scoped scheduling rail.
func BuildSchedulingDelivery(recipientKp []byte, senderActorId []byte, imipRfc5322 []byte) (SealedSchedulingDelivery, error) {
	_uniffiRV, _uniffiErr := rustCallWithError[*SchedulingDeliveryError](FfiConverterSchedulingDeliveryError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_build_scheduling_delivery(FfiConverterBytesINSTANCE.Lower(recipientKp), FfiConverterBytesINSTANCE.Lower(senderActorId), FfiConverterBytesINSTANCE.Lower(imipRfc5322), _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue SealedSchedulingDelivery
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterSealedSchedulingDeliveryINSTANCE.Lift(_uniffiRV), nil
	}
}

// The full header line (no trailing CRLF) for the Go MTA's `prependHeaders`
// — the doors' UniFFI form, beside `build_received_header`. Empty when the
// address cannot be stamped, which the Go side treats as "prepend nothing":
// a door that could not name its sender leaves the copy unstamped, and an
// unstamped copy is refused downstream, never admitted.
func BuildAuthenticatedSenderStamp(addr string) string {
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_build_authenticated_sender_stamp(FfiConverterStringINSTANCE.Lower(addr), _uniffiStatus),
		}
	}))
}

// Add the deployment-wide unlisted-recipient penalty to a combined milli-score.
//
// The recipient-whitelist model treats mail to any address **not** on the
// user's exact-alias whitelist (a catch-all match — never-registered or
// dropped) as spam: `penalty_points` (a `SpamPolicyThresholds` admin knob,
// default 0) is added to the combined score as `points * 1000` before the
// disposition decision (`mail-spam.md` § Unlisted-recipient penalty). The Go
// MTA per-recipient loop calls this (via UniFFI) only for a recipient whose
// delivery carries the `X-Fauna-Address-Catchall` stamp; a listed recipient
// passes `penalty_points = 0` (or is never called), so its score is unchanged.
//
// `penalty_points = 0` is a no-op (the opt-in-off default). A large penalty
// (e.g. `1000`) dominates any tier → guaranteed Junk. Saturating: a pathologic
// penalty clamps at `i32::MAX` rather than wrapping.
func ApplyUnlistedRecipientPenaltyMilli(combinedMilli int32, penaltyPoints uint32) int32 {
	return FfiConverterInt32INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int32_t {
		return C.uniffi_fauna_mail_fn_func_apply_unlisted_recipient_penalty_milli(FfiConverterInt32INSTANCE.Lower(combinedMilli), FfiConverterUint32INSTANCE.Lower(penaltyPoints), _uniffiStatus)
	}))
}

// Combine the deployment-wide rspamd score with the per-user Bayesian score.
//
// `max(rspamd_scaled_milli, weighted_bayesian_milli)`, floored at 0. Both
// inputs are in milli-units of the 0–15 scale (dag-cbor forbids floats). The
// `max` captures the "at least one classifier flags it" semantic: a peer
// claiming a message is legitimate (low rspamd) must not cancel the user's own
// "I've marked many like this as spam" (high Bayesian), and vice-versa.
//
// At the inbound-MX perimeter, `weighted_bayesian_milli` is always `0` —
// permanently, not a cold-start gap — because that position has no unwrap
// capability for a per-user secret (`mail-spam.md` § Scoring placement). The
// Bayesian weighting (`bayesian * confidence_factor * bayesian_weight`,
// `mail-spam.md` § Combined-score formula) is computed by the authenticated
// agent that scores post-delivery instead; this function only takes the
// already-weighted value.
func CombinedSpamScoreMilli(rspamdScaledMilli int32, weightedBayesianMilli int32) int32 {
	return FfiConverterInt32INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int32_t {
		return C.uniffi_fauna_mail_fn_func_combined_spam_score_milli(FfiConverterInt32INSTANCE.Lower(rspamdScaledMilli), FfiConverterInt32INSTANCE.Lower(weightedBayesianMilli), _uniffiStatus)
	}))
}

// Decide the delivery disposition from the combined spam score.
//
// A DMARC Quarantine-policy fail short-circuits to `PolicyJunk` when the policy
// honors it (the sender's published intent). Otherwise the combined score is
// compared against the policy's enabled tiers (`0 = disabled`), highest first:
// `reject` → `spam_folder` → else `Accept` (INBOX).
//
// `combined_score_milli` is in milli-units of the 0–15 scale; thresholds are
// integer points, so the comparison scales the threshold to milli.
//
// Gated on the full `spam` feature: it consumes `crate::auth::AuthVerdicts` for
// the DMARC short-circuit, so it is native-only (the WASM-safe `spam-classifier`
// surface stops at `combined_spam_score_milli`).
func DecideSpamDisposition(combinedScoreMilli int32, verdicts fauna_core.AuthVerdicts, policy SpamPolicy) SpamDisposition {
	return FfiConverterSpamDispositionINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_decide_spam_disposition(FfiConverterInt32INSTANCE.Lower(combinedScoreMilli),
				CFromRustBuffer(fauna_core.FfiConverterAuthVerdictsINSTANCE.LowerExternal(verdicts)), FfiConverterSpamPolicyINSTANCE.Lower(policy), _uniffiStatus),
		}
	}))
}

// Apply one agent-side `\Junk`-train event to a **decoded (plaintext)** per-user
// spam model — the mutate step of the Go MDA training a *sealed* model (leg 2 of
// the tier-1 at-rest sealing end-game, `mail-spam.md`).
//
// Once the model is sealed at rest the nest can no longer read-mutate-write it, so
// the MDA does it agent-side: it opens the sealed model under its session MLS
// capability, calls this to apply the training delta, then **re-seals**
// `new_model_bytes` to the actor's own recipient key and **seals** `delta_json`
// into a `spam_training_history` row (via `put_spam_model`'s `history_op`).
//
// Pure — no crypto, no I/O (the open/re-seal live Go-side). Byte-for-byte
// equivalent to the nest's `train_spam`/`train_ham` and the client
// `ModelWriteOp::Train` (same `SpamModel` primitives): decode
// (unreadable/empty ⇒ a fresh model, matching `load_or_create`), apply the forward
// delta, size-cap ([`MODEL_MAX_BYTES_DEFAULT`] — the client/agent is the only
// place the cap fires since the sealed blob is nest-opaque), re-serialize.
// `is_spam` selects the label ([`SpamLabel::Spam`] vs [`SpamLabel::Ham`] — a
// `\Junk`-move-in vs -out).
func ApplySpamTraining(modelBytes []byte, text string, isSpam bool) SpamTrainingMutation {
	return FfiConverterSpamTrainingMutationINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_apply_spam_training(FfiConverterBytesINSTANCE.Lower(modelBytes), FfiConverterStringINSTANCE.Lower(text), FfiConverterBoolINSTANCE.Lower(isSpam), _uniffiStatus),
		}
	}))
}

// The § Combined-score formula default knobs (`bayesian_weight = 0.7`,
// `min_samples = 50`, `full_confidence_samples = 200`). The catalog defaults an
// off-nest caller uses when it has no admin-effective `SpamPolicyThresholds`
// snapshot yet (the MDA seeds the projected Tier-2 `mail.spam.bayesian_*`
// overrides via `mailfauna.BayesianKnobsFromSnapshot`).
func DefaultBayesianKnobs() BayesianKnobs {
	return FfiConverterBayesianKnobsINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_default_bayesian_knobs(_uniffiStatus),
		}
	}))
}

// Fold the published deployment baseline into a just-unwrapped per-user model
// at the **scoring agent** — the client / AUTH'd MDA session leg of the
// read-time faded prior (`mail-spam.md` § Cold start Path 2 step 4). The
// baseline arrives on the `fetch_spam_model` reply's additive `baseline`
// field **only for a client-sealed stored model** (the nest folds a
// plaintext-stored one itself — the no-double-fold rule, `mail-spam.md`
// § Encrypted-mode interaction); the caller passes it through here after
// unwrapping the model. Delegates to [`SpamModel::fold_baseline_faded`], the
// one fade implementation every position shares, so scores stay
// byte-identical. Tolerant: an empty or unparseable baseline (or an
// unparseable model, or a model at/above `full_confidence_samples`) returns
// `model_bytes` unchanged.
func FoldSpamModelBaseline(modelBytes []byte, baselineBytes []byte, fullConfidenceSamples uint32) []byte {
	return FfiConverterBytesINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_fold_spam_model_baseline(FfiConverterBytesINSTANCE.Lower(modelBytes), FfiConverterBytesINSTANCE.Lower(baselineBytes), FfiConverterUint32INSTANCE.Lower(fullConfidenceSamples), _uniffiStatus),
		}
	}))
}

// On-device per-user spam score from a serialized model.
//
// `model_bytes` is the opaque `SpamModel::to_bytes` serde_json a client / the
// MDA fetches from nest (the `spam_models` row). Decodes it (unreadable/empty ⇒
// a fresh empty model), scores `text`, applies the confidence ramp + Bayesian
// weight, and returns the `weighted_bayesian_milli` term ready to pass to
// [`combined_spam_score_milli`](super::combined_spam_score_milli)
// (`mail-spam.md` § Combined-score formula). A fresh or sub-`min_samples` model
// returns 0 (the cold-start clamp), so an untrained user contributes nothing.
func WeightedBayesianMilliForModel(modelBytes []byte, text string, knobs BayesianKnobs) int32 {
	return FfiConverterInt32INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.int32_t {
		return C.uniffi_fauna_mail_fn_func_weighted_bayesian_milli_for_model(FfiConverterBytesINSTANCE.Lower(modelBytes), FfiConverterStringINSTANCE.Lower(text), FfiConverterBayesianKnobsINSTANCE.Lower(knobs), _uniffiStatus)
	}))
}

// UniFFI: open a rejoined staged envelope (the Go MTA's outbound-due
// consumer leg).
func OpenStagedBody(sealed []byte, key []byte) ([]byte, error) {
	_uniffiRV, _uniffiErr := rustCallWithError[*StagedEnvelopeFfiError](FfiConverterStagedEnvelopeFfiError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_open_staged_body(FfiConverterBytesINSTANCE.Lower(sealed), FfiConverterBytesINSTANCE.Lower(key), _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue []byte
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterBytesINSTANCE.Lift(_uniffiRV), nil
	}
}

// UniFFI: seal a plaintext body for staging (the Go MTA's outbound
// enqueue leg; the split into upload chunks rides the existing
// `split_sealed_mail_body` export).
func SealStagedBody(plain []byte) StagedSeal {
	return FfiConverterStagedSealINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_seal_staged_body(FfiConverterBytesINSTANCE.Lower(plain), _uniffiStatus),
		}
	}))
}

func Tokenize(input string) CanonicalTokenSet {
	return FfiConverterCanonicalTokenSetINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_fauna_mail_fn_func_tokenize(FfiConverterStringINSTANCE.Lower(input), _uniffiStatus),
		}
	}))
}

// The effective SMTP perimeter ceiling: the admin's `max_message_bytes` knob,
// and nothing else.
//
// Since ceiling retirement (2026-07-18 continuation-write flip + this slice)
// the product ceiling is `max_message_bytes` *alone* — there is no longer an
// at-rest limit to clamp against, because continuation records rest a body of
// any size. This function remains the single source the SMTP `Data` paths, the
// EHLO `SIZE` advertisement, nest-side import/APPEND admission, and
// `fauna.email.send` read, so those five enforcement points can never disagree
// on the ceiling (`docs/goal/behavior/smtp-server.md` § Message size limits).
//
// A `0` knob (no snapshot yet) does **not** mean uncapped: go-smtp treats a `0`
// `MaxMessageBytes` as unlimited, so `0` falls back to [`DEFAULT_MAX_MESSAGE_BYTES`]
// (the shipped product default), never to 0 and never to ∞.
//
// Every other knob is returned as is: the nest refuses a write above
// [`MAX_MESSAGE_BYTES_CEILING`], so no stored knob exceeds it.
func EffectiveMaxRawMessageBytes(maxMessageBytes uint32) uint32 {
	return FfiConverterUint32INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint32_t {
		return C.uniffi_fauna_mail_fn_func_effective_max_raw_message_bytes(FfiConverterUint32INSTANCE.Lower(maxMessageBytes), _uniffiStatus)
	}))
}

// Uniffi getter for [`INLINE_MAIL_REQUEST_BUDGET_BYTES`].
func InlineMailRequestBudgetBytes() uint32 {
	return FfiConverterUint32INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint32_t {
		return C.uniffi_fauna_mail_fn_func_inline_mail_request_budget_bytes(_uniffiStatus)
	}))
}

// Uniffi getter for [`MAX_INLINE_RAW_MESSAGE_BYTES`] (Go/Kotlin/Swift/C#
// bindings cannot import Rust consts).
func MaxInlineRawMessageBytes() uint32 {
	return FfiConverterUint32INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint32_t {
		return C.uniffi_fauna_mail_fn_func_max_inline_raw_message_bytes(_uniffiStatus)
	}))
}

// Uniffi getter for [`MAX_MESSAGE_BYTES_CEILING`] — the Go mail bridge reads it
// to pin the scan sidecar's shipped limits against the same number the nest
// refuses above.
func MaxMessageBytesCeiling() uint32 {
	return FfiConverterUint32INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint32_t {
		return C.uniffi_fauna_mail_fn_func_max_message_bytes_ceiling(_uniffiStatus)
	}))
}
