package com.fauna.app.data.db

import androidx.room.*

@Entity(tableName = "cached_accounts")
data class CachedAccount(
    @PrimaryKey val actorId: String,
    val nodeUrl: String,
    val handle: String,
    val domain: String,
    val tier: String,
    val profileSeq: Int = 0
)

@Entity(tableName = "conversations")
data class Conversation(
    @PrimaryKey val postId: String,
    val fromActorId: String,
    val toActorId: String,
    val subject: String,
    val body: String,
    val timestamp: Long,
    val signatureValid: Boolean,
    val encrypted: Boolean,
    val senderNode: String? = null,
    val sentByMe: Boolean = false,
    val read: Boolean = false
)

@Entity(tableName = "conversation_attachments")
data class ConversationAttachment(
    @PrimaryKey(autoGenerate = true) val id: Long = 0,
    val conversationPostId: String,
    val hash: String,
    val mediaType: String,
    val sizeBytes: Long
)

enum class SyncFileState {
    SYNCED, LOCAL_ONLY, REMOTE_ONLY, UPLOADING, DOWNLOADING, CONFLICT
}

@Entity(tableName = "sync_files")
data class SyncFile(
    @PrimaryKey val path: String,
    val folder: String,
    val sizeBytes: Long,
    val manifestHash: String,
    val localHash: String? = null,
    val state: SyncFileState = SyncFileState.REMOTE_ONLY,
    val uploadProgress: Float = 0f,
    val downloadProgress: Float = 0f,
    val updatedAt: Long,
    val localPath: String? = null
)

@Entity(tableName = "photo_backup_records")
data class PhotoBackupRecord(
    @PrimaryKey val mediaStoreId: Long,
    val contentUri: String,
    val manifestHash: String? = null,
    val sizeBytes: Long,
    val mediaType: String,
    val creationDate: Long,
    val state: SyncFileState = SyncFileState.LOCAL_ONLY,
    val remotePath: String? = null
)

@Entity(tableName = "sync_anchors")
data class SyncAnchor(
    @PrimaryKey val folder: String,
    val lastSeq: Int = 0,
    val lastSyncDate: Long = 0
)

/**
 * A SAF tree whose files ingest into one folder. [folderId] is the set's
 * `FolderRef` wire string — the row's only key (`on-demand-files.md` § Hosting
 * multiple on-demand folders), so a rename of the set keeps the directory
 * feeding it; [folder] is the name at pick time, a display label only. The ref
 * is required at rest (`NOT NULL` since schema 9): no row exists without one,
 * and nothing resolves a set from its label.
 */
@Entity(tableName = "watched_directories")
data class WatchedDirectory(
    @PrimaryKey val treeUri: String,
    val displayName: String,
    val folder: String,
    val enabled: Boolean = true,
    val folderId: String
)

@Entity(tableName = "contacts")
data class Contact(
    @PrimaryKey val peerId: String,
    val status: String,
    val handle: String? = null,
    val domain: String? = null,
    val nodeUrl: String? = null,
    val acceptedAt: Long? = null,
    val createdAt: Long? = null
)

@Entity(tableName = "knocks")
data class Knock(
    @PrimaryKey val id: Int,
    val sender: String,
    val senderNode: String,
    val summary: String,
    val createdAt: Long
)

@Entity(tableName = "mls_channels")
data class MlsChannel(
    @PrimaryKey val peerActorId: String,
    val channelId: String,
    val createdAt: Long = System.currentTimeMillis()
)

@Entity(tableName = "muted_conversations")
data class MutedConversation(
    @PrimaryKey val conversationId: String,
    val mutedAt: Long = System.currentTimeMillis()
)
