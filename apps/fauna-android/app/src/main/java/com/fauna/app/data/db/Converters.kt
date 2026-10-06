package com.fauna.app.data.db

import androidx.room.TypeConverter

class Converters {
    @TypeConverter
    fun fromSyncFileState(value: SyncFileState): String = value.name

    @TypeConverter
    fun toSyncFileState(value: String): SyncFileState = SyncFileState.valueOf(value)
}
