package com.fauna.app.core

import com.fauna.app.data.db.FaunaDatabase
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * A watched directory holds the set it ingests into by REF — the row's only key
 * (`on-demand-files.md` § Hosting multiple on-demand folders) — and a row
 * without one cannot be stored: the column is `NOT NULL` at rest. Pinned
 * against Room's own exported schema, as text: no unit test on an ARM64 dev box
 * can open a SQLite database (Robolectric's native runtime has no aarch64
 * build).
 */
class WatchedDirectoryFolderIdTest {
    private val schemaDir = File("schemas/com.fauna.app.data.db.FaunaDatabase")
    private val latest =
        FaunaDatabase.MIGRATIONS.lastOrNull()?.endVersion ?: FaunaDatabase.BASELINE_VERSION

    private fun watchedDirectories(version: Int): JsonObject {
        val database = Json.parseToJsonElement(File(schemaDir, "$version.json").readText())
            .jsonObject.getValue("database").jsonObject
        assertEquals(version, database.getValue("version").jsonPrimitive.int)
        return database.getValue("entities").jsonArray
            .map { it.jsonObject }
            .single { it.getValue("tableName").jsonPrimitive.content == "watched_directories" }
    }

    private fun createSql(table: JsonObject, name: String) =
        table.getValue("createSql").jsonPrimitive.content.replace("\${TABLE_NAME}", name)

    @Test
    fun theRefIsNotNullInTheLatestExportedSchema() {
        val folderId = watchedDirectories(latest).getValue("fields").jsonArray
            .map { it.jsonObject }
            .single { it.getValue("columnName").jsonPrimitive.content == "folderId" }
        assertTrue(folderId.getValue("notNull").jsonPrimitive.boolean)
        assertFalse("a schema newer than the migration list exists", File(schemaDir, "${latest + 1}.json").exists())
    }

    @Test
    fun theMigrationListIsOneUnbrokenChainFromTheBaseline() {
        val steps = FaunaDatabase.MIGRATIONS.map { it.startVersion to it.endVersion }
        assertEquals((FaunaDatabase.BASELINE_VERSION until latest).map { it to it + 1 }, steps)
        assertFalse(
            "a schema older than the baseline is still exported",
            File(schemaDir, "${FaunaDatabase.BASELINE_VERSION - 1}.json").exists()
        )
    }
}
