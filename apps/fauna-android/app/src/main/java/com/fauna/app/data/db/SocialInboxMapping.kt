package com.fauna.app.data.db

import com.fauna.ffi.FfiContactItem
import com.fauna.ffi.FfiKnockItem

// Social-inbox WS-RPC seam: the UniFFI mirrors of
// fauna_protocol::contacts::{KnockItem, ContactItem} (vended over
// FfiContactsClient) → the Room entities the contacts/knocks UI binds to.
// The migration off the deleted HTTP twins is confined to ApiClient's call
// sites + ContactsVM; these maps keep the entity shapes unchanged, so it is
// behaviour-preserving for every consumed field. Mirrors apple
// Core/SocialInboxFFIMapping.swift. Pure functions, so the mapping
// is unit-testable without loading the native FFI library.

/**
 * Pending-knock roster row → the [Knock] entity. The wire `id` is an i64 (a DB
 * row id); [Knock.id] is an Int — the deleted HTTP twin's `KnockResponse.id`
 * was an Int too, so the narrowing is behaviour-preserving.
 */
fun FfiKnockItem.toKnockEntity(): Knock = Knock(
    id = id.toInt(),
    sender = sender,
    senderNode = senderNode,
    summary = summary,
    createdAt = createdAt,
)

/**
 * Contact roster row → the [Contact] entity. The FFI shape now carries enriched
 * [FfiContactItem.handle]/[FfiContactItem.domain] for local peers (`None` for
 * federated ones — contacts.md § State & data shape), so the caller threads
 * through the value it prefers — the enriched field, a locally-cached value, or a
 * freshly-resolved one. [nodeUrl] is still caller-supplied (the FFI shape has none).
 */
fun FfiContactItem.toContactEntity(
    handle: String? = null,
    domain: String? = null,
    nodeUrl: String? = null,
): Contact = Contact(
    peerId = peerId,
    status = status,
    handle = handle,
    domain = domain,
    nodeUrl = nodeUrl,
    acceptedAt = acceptedAt,
    createdAt = createdAt,
)
