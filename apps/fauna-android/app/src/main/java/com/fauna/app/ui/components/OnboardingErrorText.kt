package com.fauna.app.ui.components

import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import social.fauna.generated.Ids

@Composable
fun OnboardingErrorText(error: String?, modifier: Modifier = Modifier) {
    if (error != null) {
        Text(
            text = error,
            color = MaterialTheme.colorScheme.error,
            modifier = modifier.padding(top = 16.dp).testTag(Ids.ERROR_MESSAGE)
        )
    }
}
