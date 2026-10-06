package com.fauna.app.di

import com.fauna.app.core.AccountStores
import com.fauna.app.data.db.FaunaDatabase
import com.fauna.app.data.db.MutedConversationDao
import dagger.Module
import dagger.Provides
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent
import okhttp3.OkHttpClient
import java.util.concurrent.TimeUnit
import javax.inject.Singleton

@Module
@InstallIn(SingletonComponent::class)
object AppModule {

    // The BASE client: timeouts and the shared pool, strict WebPKI. It grants a
    // self-signed nest nothing — `ApiClient` derives the trust-deciding client
    // from it (`NestCertTrust.makeClient`), so anything else injecting this one
    // gets the OS trust store alone, never a carve-out it did not ask for.
    @Provides
    @Singleton
    fun provideOkHttpClient(): OkHttpClient =
        OkHttpClient.Builder()
            .connectTimeout(30, TimeUnit.SECONDS)
            .readTimeout(60, TimeUnit.SECONDS)
            .writeTimeout(60, TimeUnit.SECONDS)
            .build()

    // Deliberately NOT @Singleton — the account-scoped database handle has to be
    // closeable and re-openable (account-scoping.md § Erasure follows scope), and
    // a cached binding would hand every consumer built after a sign-out the same
    // closed instance. AccountStores owns the caching AND the lifecycle; this just
    // asks it for the current handle, so a consumer constructed after an erase
    // gets a fresh, empty store.
    @Provides
    fun provideDatabase(accountStores: AccountStores): FaunaDatabase = accountStores.database()

    @Provides
    fun provideConversationDao(db: FaunaDatabase) = db.conversationDao()

    @Provides
    fun provideSyncFileDao(db: FaunaDatabase) = db.syncFileDao()

    @Provides
    fun providePhotoBackupDao(db: FaunaDatabase) = db.photoBackupDao()

    @Provides
    fun provideSyncAnchorDao(db: FaunaDatabase) = db.syncAnchorDao()

    @Provides
    fun provideWatchedDirectoryDao(db: FaunaDatabase) = db.watchedDirectoryDao()

    @Provides
    fun provideCachedAccountDao(db: FaunaDatabase) = db.cachedAccountDao()

    @Provides
    fun provideContactDao(db: FaunaDatabase) = db.contactDao()

    @Provides
    fun provideKnockDao(db: FaunaDatabase) = db.knockDao()

    @Provides
    fun provideMlsChannelDao(db: FaunaDatabase) = db.mlsChannelDao()

    @Provides
    fun provideMutedConversationDao(db: FaunaDatabase): MutedConversationDao = db.mutedConversationDao()
}
