package com.fauna.app.core

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.fauna_core.MutedKeyword

/**
 * The process-held muted-word list (`behavior/notifications.md` § Muted words →
 * *Where logic lives*): a refresh publishes what the store reads, a failed
 * re-read keeps the list (a store hiccup must not un-collapse every row), a
 * Settings write's answer is published as is, and an ended identity empties
 * it. FFI-free.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class MutedWordsHolderTest {

    private fun words(vararg terms: String) = terms.map { MutedKeyword(keyword = it, weight = -1000L) }

    @Test
    fun aRefreshPublishesAndAFailedReadKeepsTheList() = runTest {
        val answers = ArrayDeque<() -> List<MutedKeyword>>(
            listOf({ words("spoiler") }, { words("spoiler", "finale") }, { throw RuntimeException("store unreadable") }),
        )
        val holder = MutedWordsHolder(CoroutineScope(UnconfinedTestDispatcher(testScheduler))) {
            answers.removeFirst()()
        }

        holder.refresh()
        assertEquals(listOf("spoiler"), holder.words.value.map { it.keyword })

        holder.refresh()
        assertEquals(listOf("spoiler", "finale"), holder.words.value.map { it.keyword })

        holder.refresh()
        assertEquals(
            "a failed re-read keeps the list",
            listOf("spoiler", "finale"),
            holder.words.value.map { it.keyword },
        )
    }

    @Test
    fun aPublishedListReplacesItAndClearEmptiesIt() = runTest {
        val holder = MutedWordsHolder(CoroutineScope(UnconfinedTestDispatcher(testScheduler))) { words("stale") }

        holder.publish(words("finale"))
        assertEquals(listOf("finale"), holder.words.value.map { it.keyword })

        holder.clear()
        assertEquals(emptyList<MutedKeyword>(), holder.words.value)
    }
}
