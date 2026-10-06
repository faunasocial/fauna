package com.fauna.app.testing

import uniffi.fauna_labeler_catalog_machine.LabelerCatalogEntry
import uniffi.fauna_labeler_catalog_machine.LabelerInspectListEntry
import uniffi.fauna_labeler_catalog_machine.LabelerInspectModelNgram
import uniffi.fauna_labeler_catalog_machine.LabelerInspectView

/**
 * Defaulted factories for `fauna-labeler-catalog-machine`'s UniFFI records.
 * Kotlin named-arg constructors have no defaults, so every hand-listed call
 * site breaks on an additive field-add a sibling correctly made elsewhere — one factory per record means a field-add is a
 * one-line edit here instead of N call sites. Widen these (never re-hand-list
 * a broken call site) as more records recur; don't speculatively cover
 * records that haven't broken yet.
 */
object LabelerCatalogEntryFixture {
    fun make(
        labelerId: String,
        subscribed: Boolean,
        artifactKind: String = "wasm",
        version: ULong = 1uL,
        publisherActor: String = "ab".repeat(32),
        // 0 = absent (any non-text-model kind) — NOT
        // "version zero"; only meaningful for `text-model` kind rows.
        artifactVersion: ULong = 0uL,
        contentKind: String = "post",
        factor: String = "labeler:$labelerId",
        wasmHash: String = "cd".repeat(36),
        wasmSize: ULong = 1024uL,
    ) = LabelerCatalogEntry(
        labelerId = labelerId,
        version = version,
        publisherActor = publisherActor,
        artifactKind = artifactKind,
        artifactVersion = artifactVersion,
        contentKind = contentKind,
        factor = factor,
        wasmHash = wasmHash,
        wasmSize = wasmSize,
        subscribed = subscribed,
    )
}

object LabelerInspectViewFixture {
    fun make(
        verified: Boolean = true,
        artifactKind: String = "wasm",
        listName: String? = null,
        listEntries: List<LabelerInspectListEntry> = emptyList(),
        // `text-model` kind only, the `list` pair's exact mirror — defaulted so
        // the wasm/list cases stay unchanged.
        modelName: String? = null,
        modelNgrams: List<LabelerInspectModelNgram> = emptyList(),
        labelerId: String = "a",
        version: ULong = 1uL,
        wasmHash: String = "cd".repeat(36),
        wasmSize: ULong = 1024uL,
        needsText: Boolean = true,
        needsHashtags: Boolean = false,
        needsMediaMetadata: Boolean = false,
        needsAuthor: Boolean = false,
        needsAttachmentBytes: Boolean = false,
    ) = LabelerInspectView(
        labelerId = labelerId,
        version = version,
        artifactKind = artifactKind,
        wasmHash = wasmHash,
        wasmSize = wasmSize,
        needsText = needsText,
        needsHashtags = needsHashtags,
        needsMediaMetadata = needsMediaMetadata,
        needsAuthor = needsAuthor,
        needsAttachmentBytes = needsAttachmentBytes,
        verified = verified,
        listName = listName,
        listEntries = listEntries,
        modelName = modelName,
        modelNgrams = modelNgrams,
    )
}
