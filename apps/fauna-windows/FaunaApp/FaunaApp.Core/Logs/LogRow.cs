namespace FaunaApp.Core.Logs;

/// <summary>
/// One rendered <c>log-entry</c> row for the Logs pages' list. Built from a
/// <c>uniffi.fauna_log.LogEntry</c> by <see cref="LogsFormat.Rows"/> so the XAML
/// never sees the internal UniFFI types. <see cref="Line"/> is the indexed one-line
/// form (<c>LEVEL · HH:mm:ss · target · message</c>) — the e2e <c>log-entry</c>
/// marker / copy unit; <see cref="Message"/> over <see cref="Subtitle"/>
/// (<c>LEVEL · time · target</c>) is the two-line visual row (mirrors the linux
/// message-title-over-subtitle row). <c>public</c> so the WinUI DataTemplate binds it.
/// </summary>
public sealed record LogRow(string Line, string Message, string Subtitle);
