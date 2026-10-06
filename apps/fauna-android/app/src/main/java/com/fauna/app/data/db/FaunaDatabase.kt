package com.fauna.app.data.db

import androidx.room.Database
import androidx.room.RoomDatabase
import androidx.room.TypeConverters
import androidx.room.migration.Migration

/**
 * The account-scoped Room store. **Version 9 is a baseline, not a chain head:**
 * the compat-remnant sweep (`version-compatibility.md` § Dimension 2, the fourth
 * ratified exception) collapsed the migration history into the schema the
 * entities declare, exported as
 * `schemas/com.fauna.app.data.db.FaunaDatabase/9.json`. The number continues
 * rather than restarting at 1, the same choice the nest's genesis schema made
 * (§ Dimension 1): the next schema change is `9 → 10`, ships its `Migration` in
 * the same commit in [MIGRATIONS] (registered on the one builder,
 * `AccountStores.database`), and exports its `schemas/<version>.json`. A
 * database stamped below 9 has no path here and Room refuses to open it.
 */
@Database(
    entities = [
        CachedAccount::class,
        Conversation::class,
        ConversationAttachment::class,
        SyncFile::class,
        PhotoBackupRecord::class,
        SyncAnchor::class,
        WatchedDirectory::class,
        Contact::class,
        Knock::class,
        MlsChannel::class,
        MutedConversation::class,
    ],
    version = 9,
    exportSchema = true
)
@TypeConverters(Converters::class)
abstract class FaunaDatabase : RoomDatabase() {
    abstract fun conversationDao(): ConversationDao
    abstract fun syncFileDao(): SyncFileDao
    abstract fun photoBackupDao(): PhotoBackupDao
    abstract fun syncAnchorDao(): SyncAnchorDao
    abstract fun watchedDirectoryDao(): WatchedDirectoryDao
    abstract fun cachedAccountDao(): CachedAccountDao
    abstract fun contactDao(): ContactDao
    abstract fun knockDao(): KnockDao
    abstract fun mlsChannelDao(): MlsChannelDao
    abstract fun mutedConversationDao(): MutedConversationDao

    companion object {
        /** The baseline's version — what a store with no migration to run carries. */
        const val BASELINE_VERSION = 9

        /**
         * Every migration from [BASELINE_VERSION] on, in order — the one list
         * the builder registers. Empty: the baseline is the only schema.
         */
        val MIGRATIONS: Array<Migration> = arrayOf()

        @Volatile
        private var INSTANCE: FaunaDatabase? = null

        /**
         * The live handle [com.fauna.app.core.AccountStores.database] opened, or
         * null if none is open. That accessor is the only opener — it resolves
         * the active account's scoped file; there is no unscoped `fauna.db`.
         */
        fun getInstance(): FaunaDatabase? = INSTANCE

        fun setInstance(db: FaunaDatabase) {
            INSTANCE = db
        }

        /**
         * Drop the cached handle — the account-scoped database was closed (sign-out).
         * Leaving a closed instance here would make every later `getInstance()`
         * consumer (the documents provider, the test agent) throw on a database
         * that a fresh `AccountStores.database()` would happily reopen.
         */
        fun clearInstance() {
            INSTANCE = null
        }
    }
}
