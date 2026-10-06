package com.fauna.app.core

import javax.inject.Inject
import javax.inject.Singleton

@Singleton
class FfiCryptoOps @Inject constructor() : CryptoOps {
    override fun generateKeypair(): ByteArray = com.fauna.ffi.generateKeypair()
    override fun actorIdFromSecret(secret: ByteArray): ByteArray = com.fauna.ffi.actorIdFromSecret(secret)
}
