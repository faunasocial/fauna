package com.fauna.app.data.db

import com.fauna.ffi.FfiContactItem
import com.fauna.ffi.FfiKnockItem
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

// Nest-free unit tests for the social-inbox FFI→Room mapping seam
// (SocialInboxMapping.kt). Constructing the FfiKnockItem / FfiContactItem
// records is pure Kotlin — no native FFI library load — so these run as plain
// JVM tests. Mirrors apple SocialInboxFFIMappingTests.swift.
class SocialInboxMappingTest {

    @Test
    fun knockItem_mapsAllFields_andNarrowsIdToInt() {
        val knock = FfiKnockItem(
            id = 7L,
            sender = "ab".repeat(32),
            senderNode = "node-a",
            summary = "alice wants to connect",
            createdAt = 1000L,
        ).toKnockEntity()

        assertEquals(7, knock.id)
        assertEquals("ab".repeat(32), knock.sender)
        assertEquals("node-a", knock.senderNode)
        assertEquals("alice wants to connect", knock.summary)
        assertEquals(1000L, knock.createdAt)
    }

    @Test
    fun contactItem_mapsFields_andThreadsResolvedHandleData() {
        // A federated row the nest can't enrich (handle/domain `null`); the caller
        // threads through the resolved values, which win.
        val contact = FfiContactItem(
            peerId = "11".repeat(32),
            status = "accepted",
            acceptedAt = 1234L,
            createdAt = 1200L,
            handle = null,
            domain = null,
        ).toContactEntity(handle = "alice", domain = "example.com", nodeUrl = "https://example.com")

        assertEquals("11".repeat(32), contact.peerId)
        assertEquals("accepted", contact.status)
        assertEquals("alice", contact.handle)
        assertEquals("example.com", contact.domain)
        assertEquals("https://example.com", contact.nodeUrl)
        assertEquals(1234L, contact.acceptedAt)
        assertEquals(1200L, contact.createdAt)
    }

    @Test
    fun contactItem_defaultsResolvedDataToNull_andPreservesNoneAcceptedAt() {
        val contact = FfiContactItem(
            peerId = "22".repeat(32),
            status = "blocked",
            acceptedAt = null,
            createdAt = 1100L,
            handle = null,
            domain = null,
        ).toContactEntity()

        assertNull(contact.handle)
        assertNull(contact.domain)
        assertNull(contact.nodeUrl)
        assertNull(contact.acceptedAt)
        assertEquals("blocked", contact.status)
    }
}
