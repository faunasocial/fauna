package com.fauna.app.data.db

import androidx.room.*
import kotlinx.coroutines.flow.Flow

@Dao
interface ConversationDao {
    @Query("SELECT * FROM conversations ORDER BY timestamp DESC")
    fun getAll(): Flow<List<Conversation>>

    @Insert(onConflict = OnConflictStrategy.IGNORE)
    suspend fun insert(conversation: Conversation)

    @Query("UPDATE conversations SET read = 1 WHERE postId = :postId")
    suspend fun markRead(postId: String)

    @Insert(onConflict = OnConflictStrategy.IGNORE)
    suspend fun insertAttachment(attachment: ConversationAttachment)

    @Query("SELECT * FROM conversation_attachments WHERE conversationPostId = :postId")
    suspend fun getAttachments(postId: String): List<ConversationAttachment>

    @Query("SELECT * FROM conversations WHERE postId = :postId")
    suspend fun getByPostId(postId: String): Conversation?

    @Query("SELECT COUNT(*) FROM conversations")
    fun countSync(): Int
}

@Dao
interface SyncFileDao {
    // Non-suspend, blocking query — the TestAgent e2e-state-protocol synchronous-read
    // pattern (mirrors ContactDao.countSync() / ConversationDao.countSync()); called
    // from the IO thread in TestAgent.serializeState(), never the main thread.
    @Query("SELECT * FROM sync_files ORDER BY path")
    fun getAllSync(): List<SyncFile>

    @Query("SELECT DISTINCT folder FROM sync_files")
    fun getFolders(): Flow<List<String>>

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(file: SyncFile)

    @Query("UPDATE sync_files SET uploadProgress = :progress WHERE path = :path")
    suspend fun updateUploadProgress(path: String, progress: Float)

    @Query("UPDATE sync_files SET downloadProgress = :progress WHERE path = :path")
    suspend fun updateDownloadProgress(path: String, progress: Float)

    @Query("UPDATE sync_files SET state = :state, localPath = :localPath WHERE path = :path")
    suspend fun updateState(path: String, state: SyncFileState, localPath: String? = null)

    @Query("SELECT * FROM sync_files WHERE path = :path LIMIT 1")
    suspend fun getByPath(path: String): SyncFile?

    @Query("DELETE FROM sync_files WHERE path = :path AND folder = :folder")
    suspend fun delete(path: String, folder: String)
}

@Dao
interface PhotoBackupDao {
    @Query("SELECT * FROM photo_backup_records ORDER BY creationDate DESC")
    fun getAll(): Flow<List<PhotoBackupRecord>>

    @Query("SELECT * FROM photo_backup_records WHERE state = 'LOCAL_ONLY'")
    suspend fun getPending(): List<PhotoBackupRecord>

    @Query("SELECT COUNT(*) FROM photo_backup_records WHERE state = 'SYNCED'")
    fun getSyncedCount(): Flow<Int>

    @Insert(onConflict = OnConflictStrategy.IGNORE)
    suspend fun insert(record: PhotoBackupRecord)

    // manifestHash is left NULL post-cutover: the engine host's own SyncDb owns
    // sync state now — the DAO row is only the
    // OS-asset dedup ledger (claim 8, `file-sync.md:148-160`/`:283`).
    @Query("UPDATE photo_backup_records SET state = :state, manifestHash = NULL, remotePath = :remotePath WHERE mediaStoreId = :id")
    suspend fun markSynced(id: Long, state: SyncFileState, remotePath: String)
}

@Dao
interface SyncAnchorDao {
    @Query("SELECT * FROM sync_anchors WHERE folder = :folder")
    suspend fun get(folder: String): SyncAnchor?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(anchor: SyncAnchor)
}

@Dao
interface WatchedDirectoryDao {
    @Query("SELECT * FROM watched_directories WHERE enabled = 1")
    fun getEnabled(): Flow<List<WatchedDirectory>>

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(dir: WatchedDirectory)

    @Delete
    suspend fun delete(dir: WatchedDirectory)
}

@Dao
interface CachedAccountDao {
    @Query("SELECT * FROM cached_accounts LIMIT 1")
    suspend fun get(): CachedAccount?

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(account: CachedAccount)

    @Query("DELETE FROM cached_accounts")
    suspend fun deleteAll()
}

@Dao
interface ContactDao {
    @Query("SELECT * FROM contacts ORDER BY status, handle")
    fun getAll(): Flow<List<Contact>>

    @Query("SELECT * FROM contacts WHERE status = :status ORDER BY handle")
    fun getByStatus(status: String): Flow<List<Contact>>

    @Query("SELECT * FROM contacts WHERE peerId = :peerId")
    suspend fun getByPeerId(peerId: String): Contact?

    @Query("SELECT * FROM contacts WHERE status IN ('confirmed', 'accepted') ORDER BY handle")
    fun getMessagable(): Flow<List<Contact>>

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(contact: Contact)

    @Query("DELETE FROM contacts WHERE peerId = :peerId")
    suspend fun deleteByPeerId(peerId: String)

    @Query("DELETE FROM contacts")
    suspend fun deleteAll()

    @Query("SELECT COUNT(*) FROM contacts")
    fun countSync(): Int

    // Non-suspend, blocking query — TestAgent e2e-state-protocol synchronous-read
    // pattern (mirrors countSync() above), called from the IO thread only.
    @Query("SELECT * FROM contacts ORDER BY status, handle")
    fun getAllSync(): List<Contact>
}

@Dao
interface KnockDao {
    @Query("SELECT * FROM knocks ORDER BY createdAt DESC")
    fun getAll(): Flow<List<Knock>>

    // Non-suspend, blocking query — TestAgent e2e-state-protocol synchronous-read
    // pattern (mirrors ContactDao.countSync()/getAllSync()), IO thread only.
    @Query("SELECT * FROM knocks ORDER BY createdAt DESC")
    fun getAllSync(): List<Knock>

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(knock: Knock)

    @Query("DELETE FROM knocks WHERE id = :id")
    suspend fun delete(id: Int)

    @Query("DELETE FROM knocks")
    suspend fun deleteAll()
}

@Dao
interface MlsChannelDao {
    @Query("SELECT * FROM mls_channels WHERE peerActorId = :peerId")
    suspend fun getByPeer(peerId: String): MlsChannel?

    @Query("SELECT * FROM mls_channels WHERE channelId = :channelId")
    suspend fun getByChannelId(channelId: String): MlsChannel?

    @Query("SELECT * FROM mls_channels")
    suspend fun getAll(): List<MlsChannel>

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(channel: MlsChannel)
}

@Dao
interface MutedConversationDao {
    @Query("SELECT * FROM muted_conversations")
    fun getAll(): Flow<List<MutedConversation>>

    @Query("SELECT EXISTS(SELECT 1 FROM muted_conversations WHERE conversationId = :id)")
    suspend fun isMuted(id: String): Boolean

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun mute(entry: MutedConversation)

    @Query("DELETE FROM muted_conversations WHERE conversationId = :id")
    suspend fun unmute(id: String)
}
