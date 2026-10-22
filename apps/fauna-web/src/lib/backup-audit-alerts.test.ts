// Deno test for `auditAlertReasons`. Run via:
//
//     deno test apps/fauna-web/src/lib/backup-audit-alerts.test.ts

import { auditAlertBanners, auditAlertReasons } from './backup-audit-alerts.ts';
import type { DestinationAuditRow } from '#lib/rpc';
import type { BackupAuditAlertReason } from '#lib/wasm';

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}

// Reasons are opaque to the SPA, so the fixtures only need to be distinguishable.
const overdue = { Overdue: { since_secs: 9 } } as unknown as BackupAuditAlertReason;
const regressed = { SourceRegressed: { left_secs: 5 } } as unknown as BackupAuditAlertReason;

function row(alert_reasons: BackupAuditAlertReason[], recover_offered = false): DestinationAuditRow {
  return { destination_id: 'd1', last_passed_at: null, alert_reason: null, alert_reasons, recover_offered };
}

Deno.test('no audit row yet paints nothing', () => {
  eq(auditAlertReasons(undefined), [], 'undefined row');
});

Deno.test('an empty reasons list paints nothing', () => {
  eq(auditAlertReasons(row([])), [], 'empty list');
});

Deno.test('a recovery-window reason on its own paints one banner', () => {
  eq(auditAlertReasons(row([regressed])), [regressed], 'one reason');
});

Deno.test('a standing verdict and a recovery window paint two, in the door order', () => {
  eq(auditAlertReasons(row([overdue, regressed])), [overdue, regressed], 'two reasons');
});

Deno.test('no audit row yet carries no recovery', () => {
  eq(auditAlertBanners(undefined), [], 'undefined row');
});

Deno.test('a row that does not offer the recovery marks no banner', () => {
  eq(
    auditAlertBanners(row([overdue, regressed])),
    [{ reason: overdue, recover: false }, { reason: regressed, recover: false }],
    'not offered',
  );
});

Deno.test('an offered recovery sits on the fifth reason, the door-ordered last banner', () => {
  eq(
    auditAlertBanners(row([overdue, regressed], true)),
    [{ reason: overdue, recover: false }, { reason: regressed, recover: true }],
    'offered beside a standing verdict',
  );
  eq(auditAlertBanners(row([regressed], true)), [{ reason: regressed, recover: true }], 'alone');
});
