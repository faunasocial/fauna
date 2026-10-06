package fauna_provisioning

// #include <fauna_provisioning.h>
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
		C.ffi_fauna_provisioning_rustbuffer_free(cb.inner, status)
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
		return C.ffi_fauna_provisioning_rustbuffer_from_bytes(foreign, status)
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
		return C.ffi_fauna_provisioning_uniffi_contract_version()
	})
	if bindingsContractVersion != int(scaffoldingContractVersion) {
		// If this happens try cleaning and rebuilding your project
		panic("fauna_provisioning: UniFFI contract version mismatch")
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

type ContactInfo struct {
	FirstName  string
	LastName   string
	Email      string
	Phone      string
	Address1   string
	City       string
	State      string
	PostalCode string
	Country    string
}

func (r *ContactInfo) Destroy() {
	FfiDestroyerString{}.Destroy(r.FirstName)
	FfiDestroyerString{}.Destroy(r.LastName)
	FfiDestroyerString{}.Destroy(r.Email)
	FfiDestroyerString{}.Destroy(r.Phone)
	FfiDestroyerString{}.Destroy(r.Address1)
	FfiDestroyerString{}.Destroy(r.City)
	FfiDestroyerString{}.Destroy(r.State)
	FfiDestroyerString{}.Destroy(r.PostalCode)
	FfiDestroyerString{}.Destroy(r.Country)
}

type FfiConverterContactInfo struct{}

var FfiConverterContactInfoINSTANCE = FfiConverterContactInfo{}

func (c FfiConverterContactInfo) Lift(rb RustBufferI) ContactInfo {
	return LiftFromRustBuffer[ContactInfo](c, rb)
}

func (c FfiConverterContactInfo) Read(reader io.Reader) ContactInfo {
	return ContactInfo{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterContactInfo) Lower(value ContactInfo) C.RustBuffer {
	return LowerIntoRustBuffer[ContactInfo](c, value)
}

func (c FfiConverterContactInfo) LowerExternal(value ContactInfo) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ContactInfo](c, value))
}

func (c FfiConverterContactInfo) Write(writer io.Writer, value ContactInfo) {
	FfiConverterStringINSTANCE.Write(writer, value.FirstName)
	FfiConverterStringINSTANCE.Write(writer, value.LastName)
	FfiConverterStringINSTANCE.Write(writer, value.Email)
	FfiConverterStringINSTANCE.Write(writer, value.Phone)
	FfiConverterStringINSTANCE.Write(writer, value.Address1)
	FfiConverterStringINSTANCE.Write(writer, value.City)
	FfiConverterStringINSTANCE.Write(writer, value.State)
	FfiConverterStringINSTANCE.Write(writer, value.PostalCode)
	FfiConverterStringINSTANCE.Write(writer, value.Country)
}

type FfiDestroyerContactInfo struct{}

func (_ FfiDestroyerContactInfo) Destroy(value ContactInfo) {
	value.Destroy()
}

// Result of a "Set up DNS later" provisioning run.
type DeferredDnsResult struct {
	Result               ProvisionResult
	Records              []DnsRecord
	InstructionsMarkdown string
}

func (r *DeferredDnsResult) Destroy() {
	FfiDestroyerProvisionResult{}.Destroy(r.Result)
	FfiDestroyerSequenceDnsRecord{}.Destroy(r.Records)
	FfiDestroyerString{}.Destroy(r.InstructionsMarkdown)
}

type FfiConverterDeferredDnsResult struct{}

var FfiConverterDeferredDnsResultINSTANCE = FfiConverterDeferredDnsResult{}

func (c FfiConverterDeferredDnsResult) Lift(rb RustBufferI) DeferredDnsResult {
	return LiftFromRustBuffer[DeferredDnsResult](c, rb)
}

func (c FfiConverterDeferredDnsResult) Read(reader io.Reader) DeferredDnsResult {
	return DeferredDnsResult{
		FfiConverterProvisionResultINSTANCE.Read(reader),
		FfiConverterSequenceDnsRecordINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterDeferredDnsResult) Lower(value DeferredDnsResult) C.RustBuffer {
	return LowerIntoRustBuffer[DeferredDnsResult](c, value)
}

func (c FfiConverterDeferredDnsResult) LowerExternal(value DeferredDnsResult) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[DeferredDnsResult](c, value))
}

func (c FfiConverterDeferredDnsResult) Write(writer io.Writer, value DeferredDnsResult) {
	FfiConverterProvisionResultINSTANCE.Write(writer, value.Result)
	FfiConverterSequenceDnsRecordINSTANCE.Write(writer, value.Records)
	FfiConverterStringINSTANCE.Write(writer, value.InstructionsMarkdown)
}

type FfiDestroyerDeferredDnsResult struct{}

func (_ FfiDestroyerDeferredDnsResult) Destroy(value DeferredDnsResult) {
	value.Destroy()
}

type DnsLookupResult struct {
	HasNs    bool
	TldValid bool
}

func (r *DnsLookupResult) Destroy() {
	FfiDestroyerBool{}.Destroy(r.HasNs)
	FfiDestroyerBool{}.Destroy(r.TldValid)
}

type FfiConverterDnsLookupResult struct{}

var FfiConverterDnsLookupResultINSTANCE = FfiConverterDnsLookupResult{}

func (c FfiConverterDnsLookupResult) Lift(rb RustBufferI) DnsLookupResult {
	return LiftFromRustBuffer[DnsLookupResult](c, rb)
}

func (c FfiConverterDnsLookupResult) Read(reader io.Reader) DnsLookupResult {
	return DnsLookupResult{
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterDnsLookupResult) Lower(value DnsLookupResult) C.RustBuffer {
	return LowerIntoRustBuffer[DnsLookupResult](c, value)
}

func (c FfiConverterDnsLookupResult) LowerExternal(value DnsLookupResult) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[DnsLookupResult](c, value))
}

func (c FfiConverterDnsLookupResult) Write(writer io.Writer, value DnsLookupResult) {
	FfiConverterBoolINSTANCE.Write(writer, value.HasNs)
	FfiConverterBoolINSTANCE.Write(writer, value.TldValid)
}

type FfiDestroyerDnsLookupResult struct{}

func (_ FfiDestroyerDnsLookupResult) Destroy(value DnsLookupResult) {
	value.Destroy()
}

// A DNS record to create.
type DnsRecord struct {
	RecordType string
	Name       string
	Value      string
	Ttl        uint32
	// Priority for MX records (and other record types that require it).
	Priority *uint32
}

func (r *DnsRecord) Destroy() {
	FfiDestroyerString{}.Destroy(r.RecordType)
	FfiDestroyerString{}.Destroy(r.Name)
	FfiDestroyerString{}.Destroy(r.Value)
	FfiDestroyerUint32{}.Destroy(r.Ttl)
	FfiDestroyerOptionalUint32{}.Destroy(r.Priority)
}

type FfiConverterDnsRecord struct{}

var FfiConverterDnsRecordINSTANCE = FfiConverterDnsRecord{}

func (c FfiConverterDnsRecord) Lift(rb RustBufferI) DnsRecord {
	return LiftFromRustBuffer[DnsRecord](c, rb)
}

func (c FfiConverterDnsRecord) Read(reader io.Reader) DnsRecord {
	return DnsRecord{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterOptionalUint32INSTANCE.Read(reader),
	}
}

func (c FfiConverterDnsRecord) Lower(value DnsRecord) C.RustBuffer {
	return LowerIntoRustBuffer[DnsRecord](c, value)
}

func (c FfiConverterDnsRecord) LowerExternal(value DnsRecord) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[DnsRecord](c, value))
}

func (c FfiConverterDnsRecord) Write(writer io.Writer, value DnsRecord) {
	FfiConverterStringINSTANCE.Write(writer, value.RecordType)
	FfiConverterStringINSTANCE.Write(writer, value.Name)
	FfiConverterStringINSTANCE.Write(writer, value.Value)
	FfiConverterUint32INSTANCE.Write(writer, value.Ttl)
	FfiConverterOptionalUint32INSTANCE.Write(writer, value.Priority)
}

type FfiDestroyerDnsRecord struct{}

func (_ FfiDestroyerDnsRecord) Destroy(value DnsRecord) {
	value.Destroy()
}

// A DNS zone returned by a DNS provider.
type DnsZone struct {
	Id   string
	Name string
}

func (r *DnsZone) Destroy() {
	FfiDestroyerString{}.Destroy(r.Id)
	FfiDestroyerString{}.Destroy(r.Name)
}

type FfiConverterDnsZone struct{}

var FfiConverterDnsZoneINSTANCE = FfiConverterDnsZone{}

func (c FfiConverterDnsZone) Lift(rb RustBufferI) DnsZone {
	return LiftFromRustBuffer[DnsZone](c, rb)
}

func (c FfiConverterDnsZone) Read(reader io.Reader) DnsZone {
	return DnsZone{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterDnsZone) Lower(value DnsZone) C.RustBuffer {
	return LowerIntoRustBuffer[DnsZone](c, value)
}

func (c FfiConverterDnsZone) LowerExternal(value DnsZone) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[DnsZone](c, value))
}

func (c FfiConverterDnsZone) Write(writer io.Writer, value DnsZone) {
	FfiConverterStringINSTANCE.Write(writer, value.Id)
	FfiConverterStringINSTANCE.Write(writer, value.Name)
}

type FfiDestroyerDnsZone struct{}

func (_ FfiDestroyerDnsZone) Destroy(value DnsZone) {
	value.Destroy()
}

type NestHealthResult struct {
	State NestHealthState
}

func (r *NestHealthResult) Destroy() {
	FfiDestroyerNestHealthState{}.Destroy(r.State)
}

type FfiConverterNestHealthResult struct{}

var FfiConverterNestHealthResultINSTANCE = FfiConverterNestHealthResult{}

func (c FfiConverterNestHealthResult) Lift(rb RustBufferI) NestHealthResult {
	return LiftFromRustBuffer[NestHealthResult](c, rb)
}

func (c FfiConverterNestHealthResult) Read(reader io.Reader) NestHealthResult {
	return NestHealthResult{
		FfiConverterNestHealthStateINSTANCE.Read(reader),
	}
}

func (c FfiConverterNestHealthResult) Lower(value NestHealthResult) C.RustBuffer {
	return LowerIntoRustBuffer[NestHealthResult](c, value)
}

func (c FfiConverterNestHealthResult) LowerExternal(value NestHealthResult) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[NestHealthResult](c, value))
}

func (c FfiConverterNestHealthResult) Write(writer io.Writer, value NestHealthResult) {
	FfiConverterNestHealthStateINSTANCE.Write(writer, value.State)
}

type FfiDestroyerNestHealthResult struct{}

func (_ FfiDestroyerNestHealthResult) Destroy(value NestHealthResult) {
	value.Destroy()
}

// Final outcome of a successful run. Same shape as
// `progress::ProvisionResultPlain`; kept here for FFI surface continuity
// (this is the type historical clients see returned from the orchestrator).
type ProvisionResult struct {
	ServerId  string
	Ipv4      string
	Domain    string
	ClaimCode string
}

func (r *ProvisionResult) Destroy() {
	FfiDestroyerString{}.Destroy(r.ServerId)
	FfiDestroyerString{}.Destroy(r.Ipv4)
	FfiDestroyerString{}.Destroy(r.Domain)
	FfiDestroyerString{}.Destroy(r.ClaimCode)
}

type FfiConverterProvisionResult struct{}

var FfiConverterProvisionResultINSTANCE = FfiConverterProvisionResult{}

func (c FfiConverterProvisionResult) Lift(rb RustBufferI) ProvisionResult {
	return LiftFromRustBuffer[ProvisionResult](c, rb)
}

func (c FfiConverterProvisionResult) Read(reader io.Reader) ProvisionResult {
	return ProvisionResult{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterProvisionResult) Lower(value ProvisionResult) C.RustBuffer {
	return LowerIntoRustBuffer[ProvisionResult](c, value)
}

func (c FfiConverterProvisionResult) LowerExternal(value ProvisionResult) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ProvisionResult](c, value))
}

func (c FfiConverterProvisionResult) Write(writer io.Writer, value ProvisionResult) {
	FfiConverterStringINSTANCE.Write(writer, value.ServerId)
	FfiConverterStringINSTANCE.Write(writer, value.Ipv4)
	FfiConverterStringINSTANCE.Write(writer, value.Domain)
	FfiConverterStringINSTANCE.Write(writer, value.ClaimCode)
}

type FfiDestroyerProvisionResult struct{}

func (_ FfiDestroyerProvisionResult) Destroy(value ProvisionResult) {
	value.Destroy()
}

// FFI-friendly mirror of the orchestrator's `ProvisionResult`. The original
// has the same shape but lives in `orchestrator.rs`; this duplicate exists
// so `ProvisioningSnapshot` can derive `uniffi::Record` without pulling
// uniffi into the orchestrator types.
type ProvisionResultPlain struct {
	ServerId  string
	Ipv4      string
	Domain    string
	ClaimCode string
}

func (r *ProvisionResultPlain) Destroy() {
	FfiDestroyerString{}.Destroy(r.ServerId)
	FfiDestroyerString{}.Destroy(r.Ipv4)
	FfiDestroyerString{}.Destroy(r.Domain)
	FfiDestroyerString{}.Destroy(r.ClaimCode)
}

type FfiConverterProvisionResultPlain struct{}

var FfiConverterProvisionResultPlainINSTANCE = FfiConverterProvisionResultPlain{}

func (c FfiConverterProvisionResultPlain) Lift(rb RustBufferI) ProvisionResultPlain {
	return LiftFromRustBuffer[ProvisionResultPlain](c, rb)
}

func (c FfiConverterProvisionResultPlain) Read(reader io.Reader) ProvisionResultPlain {
	return ProvisionResultPlain{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterProvisionResultPlain) Lower(value ProvisionResultPlain) C.RustBuffer {
	return LowerIntoRustBuffer[ProvisionResultPlain](c, value)
}

func (c FfiConverterProvisionResultPlain) LowerExternal(value ProvisionResultPlain) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ProvisionResultPlain](c, value))
}

func (c FfiConverterProvisionResultPlain) Write(writer io.Writer, value ProvisionResultPlain) {
	FfiConverterStringINSTANCE.Write(writer, value.ServerId)
	FfiConverterStringINSTANCE.Write(writer, value.Ipv4)
	FfiConverterStringINSTANCE.Write(writer, value.Domain)
	FfiConverterStringINSTANCE.Write(writer, value.ClaimCode)
}

type FfiDestroyerProvisionResultPlain struct{}

func (_ FfiDestroyerProvisionResultPlain) Destroy(value ProvisionResultPlain) {
	value.Destroy()
}

// The full snapshot. Always carries exactly four entries in fixed order:
// Domain, Server, Dns, Online.
type ProvisioningSnapshot struct {
	Overall      OverallStatus
	Steps        []StepSnapshot
	StartedAtMs  *uint64
	FinishedAtMs *uint64
	// Set when overall == Succeeded. Mirrors the previous flow's
	// `ProvisionResult` return value.
	Result *ProvisionResultPlain
	// Set when overall == Failed or Cancelled.
	FinalError *string
}

func (r *ProvisioningSnapshot) Destroy() {
	FfiDestroyerOverallStatus{}.Destroy(r.Overall)
	FfiDestroyerSequenceStepSnapshot{}.Destroy(r.Steps)
	FfiDestroyerOptionalUint64{}.Destroy(r.StartedAtMs)
	FfiDestroyerOptionalUint64{}.Destroy(r.FinishedAtMs)
	FfiDestroyerOptionalProvisionResultPlain{}.Destroy(r.Result)
	FfiDestroyerOptionalString{}.Destroy(r.FinalError)
}

type FfiConverterProvisioningSnapshot struct{}

var FfiConverterProvisioningSnapshotINSTANCE = FfiConverterProvisioningSnapshot{}

func (c FfiConverterProvisioningSnapshot) Lift(rb RustBufferI) ProvisioningSnapshot {
	return LiftFromRustBuffer[ProvisioningSnapshot](c, rb)
}

func (c FfiConverterProvisioningSnapshot) Read(reader io.Reader) ProvisioningSnapshot {
	return ProvisioningSnapshot{
		FfiConverterOverallStatusINSTANCE.Read(reader),
		FfiConverterSequenceStepSnapshotINSTANCE.Read(reader),
		FfiConverterOptionalUint64INSTANCE.Read(reader),
		FfiConverterOptionalUint64INSTANCE.Read(reader),
		FfiConverterOptionalProvisionResultPlainINSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterProvisioningSnapshot) Lower(value ProvisioningSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[ProvisioningSnapshot](c, value)
}

func (c FfiConverterProvisioningSnapshot) LowerExternal(value ProvisioningSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ProvisioningSnapshot](c, value))
}

func (c FfiConverterProvisioningSnapshot) Write(writer io.Writer, value ProvisioningSnapshot) {
	FfiConverterOverallStatusINSTANCE.Write(writer, value.Overall)
	FfiConverterSequenceStepSnapshotINSTANCE.Write(writer, value.Steps)
	FfiConverterOptionalUint64INSTANCE.Write(writer, value.StartedAtMs)
	FfiConverterOptionalUint64INSTANCE.Write(writer, value.FinishedAtMs)
	FfiConverterOptionalProvisionResultPlainINSTANCE.Write(writer, value.Result)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.FinalError)
}

type FfiDestroyerProvisioningSnapshot struct{}

func (_ FfiDestroyerProvisioningSnapshot) Destroy(value ProvisioningSnapshot) {
	value.Destroy()
}

// Per-step retry budget. The orchestrator's `run_step` loop reads these.
type RetryPolicy struct {
	MaxAttempts      uint32
	InitialBackoffMs uint32
	MaxBackoffMs     uint32
}

func (r *RetryPolicy) Destroy() {
	FfiDestroyerUint32{}.Destroy(r.MaxAttempts)
	FfiDestroyerUint32{}.Destroy(r.InitialBackoffMs)
	FfiDestroyerUint32{}.Destroy(r.MaxBackoffMs)
}

type FfiConverterRetryPolicy struct{}

var FfiConverterRetryPolicyINSTANCE = FfiConverterRetryPolicy{}

func (c FfiConverterRetryPolicy) Lift(rb RustBufferI) RetryPolicy {
	return LiftFromRustBuffer[RetryPolicy](c, rb)
}

func (c FfiConverterRetryPolicy) Read(reader io.Reader) RetryPolicy {
	return RetryPolicy{
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
	}
}

func (c FfiConverterRetryPolicy) Lower(value RetryPolicy) C.RustBuffer {
	return LowerIntoRustBuffer[RetryPolicy](c, value)
}

func (c FfiConverterRetryPolicy) LowerExternal(value RetryPolicy) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RetryPolicy](c, value))
}

func (c FfiConverterRetryPolicy) Write(writer io.Writer, value RetryPolicy) {
	FfiConverterUint32INSTANCE.Write(writer, value.MaxAttempts)
	FfiConverterUint32INSTANCE.Write(writer, value.InitialBackoffMs)
	FfiConverterUint32INSTANCE.Write(writer, value.MaxBackoffMs)
}

type FfiDestroyerRetryPolicy struct{}

func (_ FfiDestroyerRetryPolicy) Destroy(value RetryPolicy) {
	value.Destroy()
}

// Normalized server-type info returned by `list_server_types()`. The UI
// renders these as radio options on the VPS configuration page.
type ServerTypeInfo struct {
	Id                string
	Vcpu              uint32
	MemGb             float32
	DiskGb            uint32
	PriceMonthlyCents uint64
	Currency          string
}

func (r *ServerTypeInfo) Destroy() {
	FfiDestroyerString{}.Destroy(r.Id)
	FfiDestroyerUint32{}.Destroy(r.Vcpu)
	FfiDestroyerFloat32{}.Destroy(r.MemGb)
	FfiDestroyerUint32{}.Destroy(r.DiskGb)
	FfiDestroyerUint64{}.Destroy(r.PriceMonthlyCents)
	FfiDestroyerString{}.Destroy(r.Currency)
}

type FfiConverterServerTypeInfo struct{}

var FfiConverterServerTypeInfoINSTANCE = FfiConverterServerTypeInfo{}

func (c FfiConverterServerTypeInfo) Lift(rb RustBufferI) ServerTypeInfo {
	return LiftFromRustBuffer[ServerTypeInfo](c, rb)
}

func (c FfiConverterServerTypeInfo) Read(reader io.Reader) ServerTypeInfo {
	return ServerTypeInfo{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterFloat32INSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterServerTypeInfo) Lower(value ServerTypeInfo) C.RustBuffer {
	return LowerIntoRustBuffer[ServerTypeInfo](c, value)
}

func (c FfiConverterServerTypeInfo) LowerExternal(value ServerTypeInfo) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ServerTypeInfo](c, value))
}

func (c FfiConverterServerTypeInfo) Write(writer io.Writer, value ServerTypeInfo) {
	FfiConverterStringINSTANCE.Write(writer, value.Id)
	FfiConverterUint32INSTANCE.Write(writer, value.Vcpu)
	FfiConverterFloat32INSTANCE.Write(writer, value.MemGb)
	FfiConverterUint32INSTANCE.Write(writer, value.DiskGb)
	FfiConverterUint64INSTANCE.Write(writer, value.PriceMonthlyCents)
	FfiConverterStringINSTANCE.Write(writer, value.Currency)
}

type FfiDestroyerServerTypeInfo struct{}

func (_ FfiDestroyerServerTypeInfo) Destroy(value ServerTypeInfo) {
	value.Destroy()
}

// One step's slot in the snapshot.
type StepSnapshot struct {
	Kind         ProvisionStep
	Status       StepStatus
	Substep      *SubstepKey
	Attempt      uint32
	MaxAttempts  uint32
	LastError    *string
	SkipReason   *SkipReason
	StartedAtMs  *uint64
	FinishedAtMs *uint64
	// Display projection: whether the `provisioning-substep` text row should
	// render. Whether the `provisioning-step-error` row should render. Whether
	// the `(attempt N of M)` suffix should be appended.
	//
	// These three are the *single source* of the per-step visibility rule that
	// every app used to re-derive (it had drifted — Android most of all).
	// They are populated by `ProvisioningSnapshot::enrich_display`, called by
	// the machine's `provisioning_snapshot()` getter on its outgoing clone, so
	// every app — including those across the uniffi/wasm boundary that
	// cannot call methods on a `Record` — reads identical booleans instead of
	// re-deriving the rule. **Do not read these off a snapshot obtained outside
	// that getter** (live mutated state leaves them stale; see
	// `recompute_display`). The canonical rule lives in `recompute_display`.
	//
	// `#[serde(default)]`: a producer that hasn't run `enrich_display` — only a
	// hand-built test fixture (`set_provisioning_snapshot_for_test`), since every
	// production snapshot is born through the enriching getter — still
	// deserializes (defaulting to `false`); the reader's getter recomputes them.
	ShowsSubstep       bool
	ShowsError         bool
	ShowsAttemptSuffix bool
}

func (r *StepSnapshot) Destroy() {
	FfiDestroyerProvisionStep{}.Destroy(r.Kind)
	FfiDestroyerStepStatus{}.Destroy(r.Status)
	FfiDestroyerOptionalSubstepKey{}.Destroy(r.Substep)
	FfiDestroyerUint32{}.Destroy(r.Attempt)
	FfiDestroyerUint32{}.Destroy(r.MaxAttempts)
	FfiDestroyerOptionalString{}.Destroy(r.LastError)
	FfiDestroyerOptionalSkipReason{}.Destroy(r.SkipReason)
	FfiDestroyerOptionalUint64{}.Destroy(r.StartedAtMs)
	FfiDestroyerOptionalUint64{}.Destroy(r.FinishedAtMs)
	FfiDestroyerBool{}.Destroy(r.ShowsSubstep)
	FfiDestroyerBool{}.Destroy(r.ShowsError)
	FfiDestroyerBool{}.Destroy(r.ShowsAttemptSuffix)
}

type FfiConverterStepSnapshot struct{}

var FfiConverterStepSnapshotINSTANCE = FfiConverterStepSnapshot{}

func (c FfiConverterStepSnapshot) Lift(rb RustBufferI) StepSnapshot {
	return LiftFromRustBuffer[StepSnapshot](c, rb)
}

func (c FfiConverterStepSnapshot) Read(reader io.Reader) StepSnapshot {
	return StepSnapshot{
		FfiConverterProvisionStepINSTANCE.Read(reader),
		FfiConverterStepStatusINSTANCE.Read(reader),
		FfiConverterOptionalSubstepKeyINSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterOptionalStringINSTANCE.Read(reader),
		FfiConverterOptionalSkipReasonINSTANCE.Read(reader),
		FfiConverterOptionalUint64INSTANCE.Read(reader),
		FfiConverterOptionalUint64INSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterStepSnapshot) Lower(value StepSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[StepSnapshot](c, value)
}

func (c FfiConverterStepSnapshot) LowerExternal(value StepSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[StepSnapshot](c, value))
}

func (c FfiConverterStepSnapshot) Write(writer io.Writer, value StepSnapshot) {
	FfiConverterProvisionStepINSTANCE.Write(writer, value.Kind)
	FfiConverterStepStatusINSTANCE.Write(writer, value.Status)
	FfiConverterOptionalSubstepKeyINSTANCE.Write(writer, value.Substep)
	FfiConverterUint32INSTANCE.Write(writer, value.Attempt)
	FfiConverterUint32INSTANCE.Write(writer, value.MaxAttempts)
	FfiConverterOptionalStringINSTANCE.Write(writer, value.LastError)
	FfiConverterOptionalSkipReasonINSTANCE.Write(writer, value.SkipReason)
	FfiConverterOptionalUint64INSTANCE.Write(writer, value.StartedAtMs)
	FfiConverterOptionalUint64INSTANCE.Write(writer, value.FinishedAtMs)
	FfiConverterBoolINSTANCE.Write(writer, value.ShowsSubstep)
	FfiConverterBoolINSTANCE.Write(writer, value.ShowsError)
	FfiConverterBoolINSTANCE.Write(writer, value.ShowsAttemptSuffix)
}

type FfiDestroyerStepSnapshot struct{}

func (_ FfiDestroyerStepSnapshot) Destroy(value StepSnapshot) {
	value.Destroy()
}

// Public TLD pricing quote returned by `Registrar::list_tld_pricing`.
type TldPriceQuote struct {
	Tld               string
	RegistrationCents uint64
	RenewalCents      uint64
	Currency          string
}

func (r *TldPriceQuote) Destroy() {
	FfiDestroyerString{}.Destroy(r.Tld)
	FfiDestroyerUint64{}.Destroy(r.RegistrationCents)
	FfiDestroyerUint64{}.Destroy(r.RenewalCents)
	FfiDestroyerString{}.Destroy(r.Currency)
}

type FfiConverterTldPriceQuote struct{}

var FfiConverterTldPriceQuoteINSTANCE = FfiConverterTldPriceQuote{}

func (c FfiConverterTldPriceQuote) Lift(rb RustBufferI) TldPriceQuote {
	return LiftFromRustBuffer[TldPriceQuote](c, rb)
}

func (c FfiConverterTldPriceQuote) Read(reader io.Reader) TldPriceQuote {
	return TldPriceQuote{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterTldPriceQuote) Lower(value TldPriceQuote) C.RustBuffer {
	return LowerIntoRustBuffer[TldPriceQuote](c, value)
}

func (c FfiConverterTldPriceQuote) LowerExternal(value TldPriceQuote) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[TldPriceQuote](c, value))
}

func (c FfiConverterTldPriceQuote) Write(writer io.Writer, value TldPriceQuote) {
	FfiConverterStringINSTANCE.Write(writer, value.Tld)
	FfiConverterUint64INSTANCE.Write(writer, value.RegistrationCents)
	FfiConverterUint64INSTANCE.Write(writer, value.RenewalCents)
	FfiConverterStringINSTANCE.Write(writer, value.Currency)
}

type FfiDestroyerTldPriceQuote struct{}

func (_ FfiDestroyerTldPriceQuote) Destroy(value TldPriceQuote) {
	value.Destroy()
}

// A region/datacenter returned by a VPS provider.
type VpsLocation struct {
	Id      string
	Name    string
	City    string
	Country string
}

func (r *VpsLocation) Destroy() {
	FfiDestroyerString{}.Destroy(r.Id)
	FfiDestroyerString{}.Destroy(r.Name)
	FfiDestroyerString{}.Destroy(r.City)
	FfiDestroyerString{}.Destroy(r.Country)
}

type FfiConverterVpsLocation struct{}

var FfiConverterVpsLocationINSTANCE = FfiConverterVpsLocation{}

func (c FfiConverterVpsLocation) Lift(rb RustBufferI) VpsLocation {
	return LiftFromRustBuffer[VpsLocation](c, rb)
}

func (c FfiConverterVpsLocation) Read(reader io.Reader) VpsLocation {
	return VpsLocation{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterStringINSTANCE.Read(reader),
	}
}

func (c FfiConverterVpsLocation) Lower(value VpsLocation) C.RustBuffer {
	return LowerIntoRustBuffer[VpsLocation](c, value)
}

func (c FfiConverterVpsLocation) LowerExternal(value VpsLocation) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[VpsLocation](c, value))
}

func (c FfiConverterVpsLocation) Write(writer io.Writer, value VpsLocation) {
	FfiConverterStringINSTANCE.Write(writer, value.Id)
	FfiConverterStringINSTANCE.Write(writer, value.Name)
	FfiConverterStringINSTANCE.Write(writer, value.City)
	FfiConverterStringINSTANCE.Write(writer, value.Country)
}

type FfiDestroyerVpsLocation struct{}

func (_ FfiDestroyerVpsLocation) Destroy(value VpsLocation) {
	value.Destroy()
}

type DomainStatus uint

const (
	DomainStatusUnregistered       DomainStatus = 1
	DomainStatusRegisteredNoNest   DomainStatus = 2
	DomainStatusRegisteredWithNest DomainStatus = 3
)

type FfiConverterDomainStatus struct{}

var FfiConverterDomainStatusINSTANCE = FfiConverterDomainStatus{}

func (c FfiConverterDomainStatus) Lift(rb RustBufferI) DomainStatus {
	return LiftFromRustBuffer[DomainStatus](c, rb)
}

func (c FfiConverterDomainStatus) Lower(value DomainStatus) C.RustBuffer {
	return LowerIntoRustBuffer[DomainStatus](c, value)
}

func (c FfiConverterDomainStatus) LowerExternal(value DomainStatus) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[DomainStatus](c, value))
}
func (FfiConverterDomainStatus) Read(reader io.Reader) DomainStatus {
	id := readInt32(reader)
	return DomainStatus(id)
}

func (FfiConverterDomainStatus) Write(writer io.Writer, value DomainStatus) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerDomainStatus struct{}

func (_ FfiDestroyerDomainStatus) Destroy(value DomainStatus) {
}

type NestHealthState interface {
	Destroy()
}
type NestHealthStateReachable struct {
}

func (e NestHealthStateReachable) Destroy() {
}

type NestHealthStateConnectionRefused struct {
}

func (e NestHealthStateConnectionRefused) Destroy() {
}

type NestHealthStateTimeout struct {
}

func (e NestHealthStateTimeout) Destroy() {
}

type NestHealthStateMisbehaving struct {
	Status uint16
}

func (e NestHealthStateMisbehaving) Destroy() {
	FfiDestroyerUint16{}.Destroy(e.Status)
}

type NestHealthStateMalformedBody struct {
}

func (e NestHealthStateMalformedBody) Destroy() {
}

type FfiConverterNestHealthState struct{}

var FfiConverterNestHealthStateINSTANCE = FfiConverterNestHealthState{}

func (c FfiConverterNestHealthState) Lift(rb RustBufferI) NestHealthState {
	return LiftFromRustBuffer[NestHealthState](c, rb)
}

func (c FfiConverterNestHealthState) Lower(value NestHealthState) C.RustBuffer {
	return LowerIntoRustBuffer[NestHealthState](c, value)
}

func (c FfiConverterNestHealthState) LowerExternal(value NestHealthState) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[NestHealthState](c, value))
}
func (FfiConverterNestHealthState) Read(reader io.Reader) NestHealthState {
	id := readInt32(reader)
	switch id {
	case 1:
		return NestHealthStateReachable{}
	case 2:
		return NestHealthStateConnectionRefused{}
	case 3:
		return NestHealthStateTimeout{}
	case 4:
		return NestHealthStateMisbehaving{
			FfiConverterUint16INSTANCE.Read(reader),
		}
	case 5:
		return NestHealthStateMalformedBody{}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterNestHealthState.Read()", id))
	}
}

func (FfiConverterNestHealthState) Write(writer io.Writer, value NestHealthState) {
	switch variant_value := value.(type) {
	case NestHealthStateReachable:
		writeInt32(writer, 1)
	case NestHealthStateConnectionRefused:
		writeInt32(writer, 2)
	case NestHealthStateTimeout:
		writeInt32(writer, 3)
	case NestHealthStateMisbehaving:
		writeInt32(writer, 4)
		FfiConverterUint16INSTANCE.Write(writer, variant_value.Status)
	case NestHealthStateMalformedBody:
		writeInt32(writer, 5)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterNestHealthState.Write", value))
	}
}

type FfiDestroyerNestHealthState struct{}

func (_ FfiDestroyerNestHealthState) Destroy(value NestHealthState) {
	value.Destroy()
}

// Overall provisioning state. The UI hides Cancel/Retry depending on this.
type OverallStatus uint

const (
	OverallStatusIdle      OverallStatus = 1
	OverallStatusRunning   OverallStatus = 2
	OverallStatusSucceeded OverallStatus = 3
	OverallStatusFailed    OverallStatus = 4
	OverallStatusCancelled OverallStatus = 5
)

type FfiConverterOverallStatus struct{}

var FfiConverterOverallStatusINSTANCE = FfiConverterOverallStatus{}

func (c FfiConverterOverallStatus) Lift(rb RustBufferI) OverallStatus {
	return LiftFromRustBuffer[OverallStatus](c, rb)
}

func (c FfiConverterOverallStatus) Lower(value OverallStatus) C.RustBuffer {
	return LowerIntoRustBuffer[OverallStatus](c, value)
}

func (c FfiConverterOverallStatus) LowerExternal(value OverallStatus) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[OverallStatus](c, value))
}
func (FfiConverterOverallStatus) Read(reader io.Reader) OverallStatus {
	id := readInt32(reader)
	return OverallStatus(id)
}

func (FfiConverterOverallStatus) Write(writer io.Writer, value OverallStatus) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerOverallStatus struct{}

func (_ FfiDestroyerOverallStatus) Destroy(value OverallStatus) {
}

// User-visible step in the four-step model. Step boundaries chosen so each
// step is contiguous in time and has a single failure mode.
type ProvisionStep uint

const (
	// Domain reservation: registrar.register (when buying) + dns.verify
	// to confirm the zone exists in the chosen DNS provider.
	ProvisionStepDomain ProvisionStep = 1
	// VPS reservation: DKIM keygen + cloud-init render + create_server.
	// Returns when the provider hands back the IPv4; cloud-init boot
	// continues asynchronously.
	ProvisionStepServer ProvisionStep = 2
	// All DNS publishing: A, MX, SPF, DMARC, two DKIM TXT records, PTR.
	ProvisionStepDns ProvisionStep = 3
	// Wait for `GET https://{domain}/api/v1/health` to return 200.
	ProvisionStepOnline ProvisionStep = 4
)

type FfiConverterProvisionStep struct{}

var FfiConverterProvisionStepINSTANCE = FfiConverterProvisionStep{}

func (c FfiConverterProvisionStep) Lift(rb RustBufferI) ProvisionStep {
	return LiftFromRustBuffer[ProvisionStep](c, rb)
}

func (c FfiConverterProvisionStep) Lower(value ProvisionStep) C.RustBuffer {
	return LowerIntoRustBuffer[ProvisionStep](c, value)
}

func (c FfiConverterProvisionStep) LowerExternal(value ProvisionStep) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[ProvisionStep](c, value))
}
func (FfiConverterProvisionStep) Read(reader io.Reader) ProvisionStep {
	id := readInt32(reader)
	return ProvisionStep(id)
}

func (FfiConverterProvisionStep) Write(writer io.Writer, value ProvisionStep) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerProvisionStep struct{}

func (_ FfiDestroyerProvisionStep) Destroy(value ProvisionStep) {
}

// Outcome of `Registrar::availability` — the structured answer the wizard's
// `dns-status-text` consumes. Distinct from `DomainAvailability` (the raw
// API response shape) because it folds the "registrar can't quote this TLD"
// case into its own variant, which `DomainAvailability` can't express.
type RegistrarAvailability interface {
	Destroy()
}

// Registrar will sell this domain. `price_cents` is what the wizard
// shows in the price-confirm step and passes back as `agreed_price_cents`
// to `register()`.
type RegistrarAvailabilityBuyable struct {
	PriceCents   uint64
	Currency     *string
	RenewalCents *uint64
}

func (e RegistrarAvailabilityBuyable) Destroy() {
	FfiDestroyerUint64{}.Destroy(e.PriceCents)
	FfiDestroyerOptionalString{}.Destroy(e.Currency)
	FfiDestroyerOptionalUint64{}.Destroy(e.RenewalCents)
}

// Domain isn't available at this registrar — either it's already
// registered (anywhere) or this registrar's API said `available: false`
// without further detail.
type RegistrarAvailabilityUnavailable struct {
}

func (e RegistrarAvailabilityUnavailable) Destroy() {
}

// This registrar doesn't carry the domain's TLD. Distinct from
// `Unavailable` so the wizard can surface "this registrar can't sell
// you a .example domain — try another" rather than "already taken."
type RegistrarAvailabilityTldNotSupported struct {
}

func (e RegistrarAvailabilityTldNotSupported) Destroy() {
}

type FfiConverterRegistrarAvailability struct{}

var FfiConverterRegistrarAvailabilityINSTANCE = FfiConverterRegistrarAvailability{}

func (c FfiConverterRegistrarAvailability) Lift(rb RustBufferI) RegistrarAvailability {
	return LiftFromRustBuffer[RegistrarAvailability](c, rb)
}

func (c FfiConverterRegistrarAvailability) Lower(value RegistrarAvailability) C.RustBuffer {
	return LowerIntoRustBuffer[RegistrarAvailability](c, value)
}

func (c FfiConverterRegistrarAvailability) LowerExternal(value RegistrarAvailability) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[RegistrarAvailability](c, value))
}
func (FfiConverterRegistrarAvailability) Read(reader io.Reader) RegistrarAvailability {
	id := readInt32(reader)
	switch id {
	case 1:
		return RegistrarAvailabilityBuyable{
			FfiConverterUint64INSTANCE.Read(reader),
			FfiConverterOptionalStringINSTANCE.Read(reader),
			FfiConverterOptionalUint64INSTANCE.Read(reader),
		}
	case 2:
		return RegistrarAvailabilityUnavailable{}
	case 3:
		return RegistrarAvailabilityTldNotSupported{}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterRegistrarAvailability.Read()", id))
	}
}

func (FfiConverterRegistrarAvailability) Write(writer io.Writer, value RegistrarAvailability) {
	switch variant_value := value.(type) {
	case RegistrarAvailabilityBuyable:
		writeInt32(writer, 1)
		FfiConverterUint64INSTANCE.Write(writer, variant_value.PriceCents)
		FfiConverterOptionalStringINSTANCE.Write(writer, variant_value.Currency)
		FfiConverterOptionalUint64INSTANCE.Write(writer, variant_value.RenewalCents)
	case RegistrarAvailabilityUnavailable:
		writeInt32(writer, 2)
	case RegistrarAvailabilityTldNotSupported:
		writeInt32(writer, 3)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterRegistrarAvailability.Write", value))
	}
}

type FfiDestroyerRegistrarAvailability struct{}

func (_ FfiDestroyerRegistrarAvailability) Destroy(value RegistrarAvailability) {
	value.Destroy()
}

// Why a step short-circuited via the pre-flight idempotency check.
type SkipReason uint

const (
	SkipReasonZoneAlreadyVerified    SkipReason = 1
	SkipReasonServerAlreadyExists    SkipReason = 2
	SkipReasonDnsRecordAlreadyExists SkipReason = 3
	SkipReasonPtrAlreadySet          SkipReason = 4
	SkipReasonNestAlreadyOnline      SkipReason = 5
)

type FfiConverterSkipReason struct{}

var FfiConverterSkipReasonINSTANCE = FfiConverterSkipReason{}

func (c FfiConverterSkipReason) Lift(rb RustBufferI) SkipReason {
	return LiftFromRustBuffer[SkipReason](c, rb)
}

func (c FfiConverterSkipReason) Lower(value SkipReason) C.RustBuffer {
	return LowerIntoRustBuffer[SkipReason](c, value)
}

func (c FfiConverterSkipReason) LowerExternal(value SkipReason) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[SkipReason](c, value))
}
func (FfiConverterSkipReason) Read(reader io.Reader) SkipReason {
	id := readInt32(reader)
	return SkipReason(id)
}

func (FfiConverterSkipReason) Write(writer io.Writer, value SkipReason) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerSkipReason struct{}

func (_ FfiDestroyerSkipReason) Destroy(value SkipReason) {
}

type StepStatus uint

const (
	StepStatusPending   StepStatus = 1
	StepStatusRunning   StepStatus = 2
	StepStatusSkipped   StepStatus = 3
	StepStatusSucceeded StepStatus = 4
	StepStatusFailed    StepStatus = 5
)

type FfiConverterStepStatus struct{}

var FfiConverterStepStatusINSTANCE = FfiConverterStepStatus{}

func (c FfiConverterStepStatus) Lift(rb RustBufferI) StepStatus {
	return LiftFromRustBuffer[StepStatus](c, rb)
}

func (c FfiConverterStepStatus) Lower(value StepStatus) C.RustBuffer {
	return LowerIntoRustBuffer[StepStatus](c, value)
}

func (c FfiConverterStepStatus) LowerExternal(value StepStatus) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[StepStatus](c, value))
}
func (FfiConverterStepStatus) Read(reader io.Reader) StepStatus {
	id := readInt32(reader)
	return StepStatus(id)
}

func (FfiConverterStepStatus) Write(writer io.Writer, value StepStatus) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerStepStatus struct{}

func (_ FfiDestroyerStepStatus) Destroy(value StepStatus) {
}

// Typed sub-step keys; the UI maps each to the localized string from
// `i18n/strings/en.yaml` under `onboarding.provision.substep`.
type SubstepKey uint

const (
	SubstepKeyDomainCheckingAvailability SubstepKey = 1
	SubstepKeyDomainRegistering          SubstepKey = 2
	SubstepKeyDomainVerifyingZone        SubstepKey = 3
	SubstepKeyServerGeneratingDkim       SubstepKey = 4
	SubstepKeyServerCreating             SubstepKey = 5
	SubstepKeyDnsAddingDomainRecords     SubstepKey = 6
	SubstepKeyDnsAddingEmailRecords      SubstepKey = 7
	SubstepKeyDnsSettingReverseDns       SubstepKey = 8
	SubstepKeyOnlineWaiting              SubstepKey = 9
	// The claim that turns "built" into "built **and claimed**" — `Online`'s
	// final substep on the standard path, run by the machine (not this crate's
	// provider-facing orchestrator) the instant `/health` answers.
	// `docs/goal/behavior/onboarding.md` § 6 *Provisioning = build + claim*.
	SubstepKeyOnlineClaiming   SubstepKey = 10
	SubstepKeyStatusSkipped    SubstepKey = 11
	SubstepKeyStatusRetrying   SubstepKey = 12
	SubstepKeyStatusCancelling SubstepKey = 13
	SubstepKeyStatusCancelled  SubstepKey = 14
)

type FfiConverterSubstepKey struct{}

var FfiConverterSubstepKeyINSTANCE = FfiConverterSubstepKey{}

func (c FfiConverterSubstepKey) Lift(rb RustBufferI) SubstepKey {
	return LiftFromRustBuffer[SubstepKey](c, rb)
}

func (c FfiConverterSubstepKey) Lower(value SubstepKey) C.RustBuffer {
	return LowerIntoRustBuffer[SubstepKey](c, value)
}

func (c FfiConverterSubstepKey) LowerExternal(value SubstepKey) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[SubstepKey](c, value))
}
func (FfiConverterSubstepKey) Read(reader io.Reader) SubstepKey {
	id := readInt32(reader)
	return SubstepKey(id)
}

func (FfiConverterSubstepKey) Write(writer io.Writer, value SubstepKey) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerSubstepKey struct{}

func (_ FfiDestroyerSubstepKey) Destroy(value SubstepKey) {
}

// Which builds a provisioned box's automatic updater follows — the
// `vps-config-update-channel-row` choice on `vps_config`
// (`docs/goal/behavior/onboarding-provisioning.md` § 5). [`Self::image_tag`]
// is the ONE place a channel becomes an image tag; which pipeline moves each
// tag is `docs/goal/architecture/build-system.md` § Image tags & channels.
type UpdateChannel uint

const (
	// Released builds. The default.
	UpdateChannelStable UpdateChannel = 1
	// Release candidates under verification.
	UpdateChannelTest UpdateChannel = 2
	// The newest development builds, not yet verified.
	UpdateChannelDev UpdateChannel = 3
)

type FfiConverterUpdateChannel struct{}

var FfiConverterUpdateChannelINSTANCE = FfiConverterUpdateChannel{}

func (c FfiConverterUpdateChannel) Lift(rb RustBufferI) UpdateChannel {
	return LiftFromRustBuffer[UpdateChannel](c, rb)
}

func (c FfiConverterUpdateChannel) Lower(value UpdateChannel) C.RustBuffer {
	return LowerIntoRustBuffer[UpdateChannel](c, value)
}

func (c FfiConverterUpdateChannel) LowerExternal(value UpdateChannel) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[UpdateChannel](c, value))
}
func (FfiConverterUpdateChannel) Read(reader io.Reader) UpdateChannel {
	id := readInt32(reader)
	return UpdateChannel(id)
}

func (FfiConverterUpdateChannel) Write(writer io.Writer, value UpdateChannel) {
	writeInt32(writer, int32(value))
}

type FfiDestroyerUpdateChannel struct{}

func (_ FfiDestroyerUpdateChannel) Destroy(value UpdateChannel) {
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

type FfiConverterOptionalProvisionResultPlain struct{}

var FfiConverterOptionalProvisionResultPlainINSTANCE = FfiConverterOptionalProvisionResultPlain{}

func (c FfiConverterOptionalProvisionResultPlain) Lift(rb RustBufferI) *ProvisionResultPlain {
	return LiftFromRustBuffer[*ProvisionResultPlain](c, rb)
}

func (_ FfiConverterOptionalProvisionResultPlain) Read(reader io.Reader) *ProvisionResultPlain {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterProvisionResultPlainINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalProvisionResultPlain) Lower(value *ProvisionResultPlain) C.RustBuffer {
	return LowerIntoRustBuffer[*ProvisionResultPlain](c, value)
}

func (c FfiConverterOptionalProvisionResultPlain) LowerExternal(value *ProvisionResultPlain) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*ProvisionResultPlain](c, value))
}

func (_ FfiConverterOptionalProvisionResultPlain) Write(writer io.Writer, value *ProvisionResultPlain) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterProvisionResultPlainINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalProvisionResultPlain struct{}

func (_ FfiDestroyerOptionalProvisionResultPlain) Destroy(value *ProvisionResultPlain) {
	if value != nil {
		FfiDestroyerProvisionResultPlain{}.Destroy(*value)
	}
}

type FfiConverterOptionalSkipReason struct{}

var FfiConverterOptionalSkipReasonINSTANCE = FfiConverterOptionalSkipReason{}

func (c FfiConverterOptionalSkipReason) Lift(rb RustBufferI) *SkipReason {
	return LiftFromRustBuffer[*SkipReason](c, rb)
}

func (_ FfiConverterOptionalSkipReason) Read(reader io.Reader) *SkipReason {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterSkipReasonINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalSkipReason) Lower(value *SkipReason) C.RustBuffer {
	return LowerIntoRustBuffer[*SkipReason](c, value)
}

func (c FfiConverterOptionalSkipReason) LowerExternal(value *SkipReason) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*SkipReason](c, value))
}

func (_ FfiConverterOptionalSkipReason) Write(writer io.Writer, value *SkipReason) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterSkipReasonINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalSkipReason struct{}

func (_ FfiDestroyerOptionalSkipReason) Destroy(value *SkipReason) {
	if value != nil {
		FfiDestroyerSkipReason{}.Destroy(*value)
	}
}

type FfiConverterOptionalSubstepKey struct{}

var FfiConverterOptionalSubstepKeyINSTANCE = FfiConverterOptionalSubstepKey{}

func (c FfiConverterOptionalSubstepKey) Lift(rb RustBufferI) *SubstepKey {
	return LiftFromRustBuffer[*SubstepKey](c, rb)
}

func (_ FfiConverterOptionalSubstepKey) Read(reader io.Reader) *SubstepKey {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterSubstepKeyINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalSubstepKey) Lower(value *SubstepKey) C.RustBuffer {
	return LowerIntoRustBuffer[*SubstepKey](c, value)
}

func (c FfiConverterOptionalSubstepKey) LowerExternal(value *SubstepKey) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*SubstepKey](c, value))
}

func (_ FfiConverterOptionalSubstepKey) Write(writer io.Writer, value *SubstepKey) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterSubstepKeyINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalSubstepKey struct{}

func (_ FfiDestroyerOptionalSubstepKey) Destroy(value *SubstepKey) {
	if value != nil {
		FfiDestroyerSubstepKey{}.Destroy(*value)
	}
}

type FfiConverterSequenceDnsRecord struct{}

var FfiConverterSequenceDnsRecordINSTANCE = FfiConverterSequenceDnsRecord{}

func (c FfiConverterSequenceDnsRecord) Lift(rb RustBufferI) []DnsRecord {
	return LiftFromRustBuffer[[]DnsRecord](c, rb)
}

func (c FfiConverterSequenceDnsRecord) Read(reader io.Reader) []DnsRecord {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]DnsRecord, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterDnsRecordINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceDnsRecord) Lower(value []DnsRecord) C.RustBuffer {
	return LowerIntoRustBuffer[[]DnsRecord](c, value)
}

func (c FfiConverterSequenceDnsRecord) LowerExternal(value []DnsRecord) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]DnsRecord](c, value))
}

func (c FfiConverterSequenceDnsRecord) Write(writer io.Writer, value []DnsRecord) {
	if len(value) > math.MaxInt32 {
		panic("[]DnsRecord is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterDnsRecordINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceDnsRecord struct{}

func (FfiDestroyerSequenceDnsRecord) Destroy(sequence []DnsRecord) {
	for _, value := range sequence {
		FfiDestroyerDnsRecord{}.Destroy(value)
	}
}

type FfiConverterSequenceStepSnapshot struct{}

var FfiConverterSequenceStepSnapshotINSTANCE = FfiConverterSequenceStepSnapshot{}

func (c FfiConverterSequenceStepSnapshot) Lift(rb RustBufferI) []StepSnapshot {
	return LiftFromRustBuffer[[]StepSnapshot](c, rb)
}

func (c FfiConverterSequenceStepSnapshot) Read(reader io.Reader) []StepSnapshot {
	length := readInt32(reader)
	if length == 0 {
		return nil
	}
	result := make([]StepSnapshot, 0, length)
	for i := int32(0); i < length; i++ {
		result = append(result, FfiConverterStepSnapshotINSTANCE.Read(reader))
	}
	return result
}

func (c FfiConverterSequenceStepSnapshot) Lower(value []StepSnapshot) C.RustBuffer {
	return LowerIntoRustBuffer[[]StepSnapshot](c, value)
}

func (c FfiConverterSequenceStepSnapshot) LowerExternal(value []StepSnapshot) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[[]StepSnapshot](c, value))
}

func (c FfiConverterSequenceStepSnapshot) Write(writer io.Writer, value []StepSnapshot) {
	if len(value) > math.MaxInt32 {
		panic("[]StepSnapshot is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	for _, item := range value {
		FfiConverterStepSnapshotINSTANCE.Write(writer, item)
	}
}

type FfiDestroyerSequenceStepSnapshot struct{}

func (FfiDestroyerSequenceStepSnapshot) Destroy(sequence []StepSnapshot) {
	for _, value := range sequence {
		FfiDestroyerStepSnapshot{}.Destroy(value)
	}
}
