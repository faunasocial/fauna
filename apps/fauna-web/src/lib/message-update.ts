// The shell message banner's update rule — what one `fauna-message-update`
// window event does to the banner's three texts (`MessageBanner.svelte`).
//
// `error` / `warning` / `info` set that text (an empty string takes it down).
// `withdrawWarning` takes the warning down only while it still shows one of
// the listed texts: the custody leg's cleared mark (`box-recovery.md` § The
// plane-era recovery floor, *(c) The writes*) withdraws a custody warning an
// earlier run painted and never whatever else the banner shows by then — the
// web host's half of `fauna_client_config::is_custody_leg_warning`.

export interface BannerTexts {
  error: string;
  warning: string;
  info: string;
}

export interface MessageUpdate {
  error?: string;
  warning?: string;
  info?: string;
  /** Texts whose showing warning this update takes down. */
  withdrawWarning?: string[];
}

export function applyMessageUpdate(shown: BannerTexts, update: MessageUpdate): BannerTexts {
  const next = { ...shown };
  if (update.error !== undefined) next.error = update.error;
  if (update.warning !== undefined) next.warning = update.warning;
  if (update.info !== undefined) next.info = update.info;
  if (update.withdrawWarning && next.warning !== '' && update.withdrawWarning.includes(next.warning)) {
    next.warning = '';
  }
  return next;
}
