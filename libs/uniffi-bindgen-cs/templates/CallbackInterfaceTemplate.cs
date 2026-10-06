{#/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */#}

{%- let cbi = ci.get_callback_interface_definition(name).unwrap() %}
{%- let type_name = cbi|type_name(ci) %}
{%- let callback_impl_name = type_name|ffi_callback_impl %}

{%- let vtable = cbi.vtable() %}
{%- let vtable_methods = cbi.vtable_methods() %}

{%- let ffi_converter_var = format!("{}.INSTANCE", ffi_converter_name) %}
{%- let ffi_init_callback = cbi.ffi_init_callback() %}

{%- call cs::docstring(cbi, 0) %}
{{ config.access_modifier() }} interface {{ type_name }} {
    {%- for meth in cbi.methods() %}
    {%- call cs::docstring(meth, 4) %}
    {%- call cs::method_throws_annotation(meth.throws_type()) %}
    {%- match meth.return_type() %}
    {%- when Some with (return_type) %}
    {{ return_type|type_name(ci) }} {{ meth.name()|fn_name }}({% call cs::arg_list_decl(meth) %});
    {%- else %}
    void {{ meth.name()|fn_name }}({% call cs::arg_list_decl(meth) %});
    {%- endmatch %}
    {%- endfor %}
}

{% include "CallbackInterfaceImpl.cs" %}

// The ffiConverter which transforms the Callbacks in to Handles to pass to Rust.
class {{ ffi_converter_name }}: FfiConverter<{{ type_name }}, ulong> {
    public static {{ ffi_converter_name }} INSTANCE = new {{ ffi_converter_name }}();

    // A callback interface's vtable is registered with Rust only inside this
    // module's `_UniFFILib` static constructor (one of its initialization_fns).
    // That cctor runs lazily — on first access to a `_UniFFILib` member. But a
    // callback can be lowered (`Lower`/`Write`, below) as the FIRST thing any code
    // touches in this module: e.g. a UniFFI object exported from a *different*
    // module takes this callback as an argument and lowers it through this
    // converter. Lowering only inserts into `handleMap` and never touches
    // `_UniFFILib`, so without help the vtable would stay unregistered and Rust
    // would call back into it — or drop the stored foreign object — and panic
    // "Foreign pointer not set". Force this module's init the moment the converter
    // is first used. Idempotent (the cctor runs once); a no-op when the module was
    // already initialized through a same-module call.
    static {{ ffi_converter_name }}() {
        System.Runtime.CompilerServices.RuntimeHelpers.RunClassConstructor(typeof(_UniFFILib).TypeHandle);
    }

    public ConcurrentHandleMap<{{ type_name }}> handleMap = new ConcurrentHandleMap<{{ type_name }}>();

    public override ulong Lower({{ type_name }} value) {
        return handleMap.Insert(value);
    }

    public override {{ type_name }} Lift(ulong value) {
        if (handleMap.TryGet(value, out var uniffiCallback)) {
            return uniffiCallback;
        } else {
            throw new InternalException($"No callback in handlemap '{value}'");
        }
    }

    public override {{ type_name }} Read(BigEndianStream stream) {
        return Lift(stream.ReadULong());
    }

    public override int AllocationSize({{ type_name }} value) {
        return 8;
    }

    public override void Write({{ type_name }} value, BigEndianStream stream) {
        stream.WriteULong(Lower(value));
    }
}
