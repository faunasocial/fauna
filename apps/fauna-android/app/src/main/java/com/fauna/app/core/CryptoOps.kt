package com.fauna.app.core

interface CryptoOps {
    fun generateKeypair(): ByteArray
    fun actorIdFromSecret(secret: ByteArray): ByteArray
}
