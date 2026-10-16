package com.fauna.app.di

import com.fauna.app.core.*
import com.fauna.app.provider.FfiOnDemandSource
import com.fauna.app.provider.OnDemandToggle
import dagger.Binds
import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent

@Module
@InstallIn(SingletonComponent::class)
abstract class BindingsModule {
    @Binds abstract fun bindCryptoOps(impl: FfiCryptoOps): CryptoOps
    @Binds abstract fun bindSessionAccount(impl: SecureStorage): SessionAccount
    @Binds abstract fun bindOnDemandToggle(impl: FfiOnDemandSource): OnDemandToggle
}
