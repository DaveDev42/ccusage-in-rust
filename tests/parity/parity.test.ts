/**
 * Black-box CLI parity harness for ccusage-rs.
 *
 * Each test:
 *   1. Builds a fixture using fs-fixture (same as upstream tests).
 *   2. Calls the upstream TypeScript loader to get the "expected" result.
 *   3. Spawns ccusage-rs with CLAUDE_CONFIG_DIR=fixture.path and parses its JSON stdout.
 *   4. Deep-compares only the shared keys (ccusage-rs emits extra fields like totalTokens
 *      that the upstream loaders don't; we strip them before comparison).
 *
 * This file is the canonical copy. tests/parity/run.sh mounts it into the
 * ccusage submodule (third_party/ccusage/apps/ccusage/src/_parity-rs.test.ts)
 * so it can use upstream's in-source vitest setup and reach data-loader
 * via a relative import.
 *
 * Originally adapted from ccusage upstream (https://github.com/ryoppippi/ccusage,
 * MIT, (c) 2025 ryoppippi). See LICENSES/ccusage-MIT.txt.
 *
 * Run with:
 *   tests/parity/run.sh
 *
 * The RUST_BIN env var overrides the ccusage-rs binary path (default:
 * target/release/ccusage-rs at the ccusage-rs repo root).
 */

import { spawnSync } from 'node:child_process';
import { createFixture } from 'fs-fixture';
import {
	loadDailyUsageData,
	loadMonthlyUsageData,
	loadWeeklyUsageData,
	loadSessionData,
	loadSessionBlockData,
	loadSessionUsageById,
} from './data-loader.ts';
import { createISOTimestamp, createModelName, createVersion } from './_types.ts';

if (import.meta.vitest) {
	const { describe, it, expect, beforeAll } = import.meta.vitest;

	// When mounted via tests/parity/run.sh, cwd = third_party/ccusage/apps/ccusage,
	// so the ccusage-rs binary lives 4 levels up. RUST_BIN env var always wins.
	const RUST_BIN =
		process.env['RUST_BIN'] ??
		new URL('../../../../target/release/ccusage-rs', import.meta.url).pathname;

	// Helper: spawn ccusage-rs and return parsed JSON stdout
	function runRs(
		args: string[],
		fixturePath: string,
		nowOverride = '2099-01-01T00:00:00Z',
	): unknown {
		const result = spawnSync(
			RUST_BIN,
			[...args, '--json', '--offline', '--now', nowOverride],
			{
				env: {
					...process.env,
					TZ: 'UTC',
					CLAUDE_CONFIG_DIR: fixturePath,
				},
				encoding: 'utf8',
				timeout: 15_000,
			},
		);
		if (result.error) {
			throw result.error;
		}
		if (result.status !== 0) {
			throw new Error(
				`ccusage-rs exited with ${result.status}:\nstdout: ${result.stdout}\nstderr: ${result.stderr}`,
			);
		}
		return JSON.parse(result.stdout);
	}

	// Strip keys that only exist in ccusage-rs output (totalTokens) so deep-equal works
	function stripRsExtras<T>(obj: T): T {
		if (Array.isArray(obj)) {
			return obj.map(stripRsExtras) as unknown as T;
		}
		if (obj != null && typeof obj === 'object') {
			const result: Record<string, unknown> = {};
			for (const [k, v] of Object.entries(obj as Record<string, unknown>)) {
				if (k === 'totalTokens') continue;
				result[k] = stripRsExtras(v);
			}
			return result as T;
		}
		return obj;
	}

	// Deep-compare only keys present in `expected`; ignore extra keys in `actual`
	function matchSubset(actual: unknown, expected: unknown, path = ''): void {
		if (expected == null || typeof expected !== 'object') {
			expect(actual, `at ${path}`).toBe(expected);
			return;
		}
		if (Array.isArray(expected)) {
			expect(Array.isArray(actual), `at ${path}: should be array`).toBe(true);
			const actualArr = actual as unknown[];
			expect(actualArr.length, `at ${path}.length`).toBe(expected.length);
			for (let i = 0; i < expected.length; i++) {
				matchSubset(actualArr[i], expected[i], `${path}[${i}]`);
			}
			return;
		}
		const actualObj = actual as Record<string, unknown>;
		for (const [k, v] of Object.entries(expected as Record<string, unknown>)) {
			matchSubset(actualObj[k], v, `${path}.${k}`);
		}
	}

	// ─── daily ───────────────────────────────────────────────────────────────────

	describe('parity: loadDailyUsageData vs ccusage-rs daily', () => {
		it('empty data returns empty array', async () => {
			await using fixture = await createFixture({ projects: {} });

			const expected = await loadDailyUsageData({ claudePath: fixture.path });
			// ccusage-rs CLI (like upstream CLI): empty daily prints `[]` (bare array, not wrapped)
			const rs = runRs(['daily', '--order', 'desc'], fixture.path) as unknown[];

			expect(expected).toEqual([]);
			expect(rs).toEqual([]);
		});

		it('aggregates tokens and cost for a single day', async () => {
			const rows = [
				{
					timestamp: '2024-01-01T10:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-01-01T12:00:00Z',
					message: { usage: { input_tokens: 200, output_tokens: 100 } },
					costUSD: 0.02,
				},
				{
					timestamp: '2024-01-01T18:00:00Z',
					message: { usage: { input_tokens: 300, output_tokens: 150 } },
					costUSD: 0.03,
				},
			];

			await using fixture = await createFixture({
				projects: {
					project1: {
						session1: {
							'file.jsonl': rows.map((d) => JSON.stringify(d)).join('\n'),
						},
					},
				},
			});

			const expected = await loadDailyUsageData({ claudePath: fixture.path });
			const rs = runRs(['daily', '--order', 'desc'], fixture.path) as {
				daily: unknown[];
			};

			expect(expected).toHaveLength(1);
			expect(expected[0]?.date).toBe('2024-01-01');

			expect(rs.daily).toHaveLength(1);
			matchSubset(rs.daily[0], {
				date: '2024-01-01',
				inputTokens: 600,
				outputTokens: 300,
				totalCost: 0.06,
			});
		});

		it('handles cache tokens', async () => {
			const row = {
				timestamp: '2024-01-01T12:00:00Z',
				message: {
					usage: {
						input_tokens: 100,
						output_tokens: 50,
						cache_creation_input_tokens: 25,
						cache_read_input_tokens: 10,
					},
				},
				costUSD: 0.01,
			};

			await using fixture = await createFixture({
				projects: { p1: { s1: { 'f.jsonl': JSON.stringify(row) } } },
			});

			const expected = await loadDailyUsageData({ claudePath: fixture.path });
			const rs = runRs(['daily', '--order', 'desc'], fixture.path) as {
				daily: unknown[];
			};

			expect(expected[0]?.cacheCreationTokens).toBe(25);
			expect(expected[0]?.cacheReadTokens).toBe(10);

			matchSubset(rs.daily[0], {
				cacheCreationTokens: 25,
				cacheReadTokens: 10,
			});
		});

		it('filters by date range (since/until)', async () => {
			const rows = [
				{
					timestamp: '2024-01-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-01-15T12:00:00Z',
					message: { usage: { input_tokens: 200, output_tokens: 100 } },
					costUSD: 0.02,
				},
				{
					timestamp: '2024-01-31T12:00:00Z',
					message: { usage: { input_tokens: 300, output_tokens: 150 } },
					costUSD: 0.03,
				},
			];

			await using fixture = await createFixture({
				projects: {
					p1: { s1: { 'f.jsonl': rows.map((d) => JSON.stringify(d)).join('\n') } },
				},
			});

			const expected = await loadDailyUsageData({
				claudePath: fixture.path,
				since: '20240110',
				until: '20240125',
			});
			const rs = runRs(
				['daily', '--order', 'desc', '--since', '20240110', '--until', '20240125'],
				fixture.path,
			) as { daily: unknown[] };

			expect(expected).toHaveLength(1);
			expect(expected[0]?.date).toBe('2024-01-15');
			matchSubset(rs.daily[0], { date: '2024-01-15', inputTokens: 200 });
		});

		it('sorts descending by default', async () => {
			const rows = [
				{
					timestamp: '2024-01-15T12:00:00Z',
					message: { usage: { input_tokens: 200, output_tokens: 100 } },
					costUSD: 0.02,
				},
				{
					timestamp: '2024-01-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-01-31T12:00:00Z',
					message: { usage: { input_tokens: 300, output_tokens: 150 } },
					costUSD: 0.03,
				},
			];

			await using fixture = await createFixture({
				projects: { p1: { s1: { 'f.jsonl': rows.map((d) => JSON.stringify(d)).join('\n') } } },
			});

			const expected = await loadDailyUsageData({ claudePath: fixture.path }); // default desc
			const rs = runRs(['daily', '--order', 'desc'], fixture.path) as { daily: unknown[] };

			const eDates = expected.map((r) => r.date);
			const rsDates = (rs.daily as { date: string }[]).map((r) => r.date);

			expect(eDates).toEqual(['2024-01-31', '2024-01-15', '2024-01-01']);
			expect(rsDates).toEqual(eDates);
		});

		it('sorts ascending when order=asc', async () => {
			const rows = [
				{
					timestamp: '2024-01-15T12:00:00Z',
					message: { usage: { input_tokens: 200, output_tokens: 100 } },
					costUSD: 0.02,
				},
				{
					timestamp: '2024-01-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-01-31T12:00:00Z',
					message: { usage: { input_tokens: 300, output_tokens: 150 } },
					costUSD: 0.03,
				},
			];

			await using fixture = await createFixture({
				projects: { p1: { s1: { 'f.jsonl': rows.map((d) => JSON.stringify(d)).join('\n') } } },
			});

			const expected = await loadDailyUsageData({ claudePath: fixture.path, order: 'asc' });
			const rs = runRs(['daily', '--order', 'asc'], fixture.path) as { daily: unknown[] };

			const eDates = expected.map((r) => r.date);
			const rsDates = (rs.daily as { date: string }[]).map((r) => r.date);
			expect(eDates).toEqual(['2024-01-01', '2024-01-15', '2024-01-31']);
			expect(rsDates).toEqual(eDates);
		});

		it('skips invalid JSON lines gracefully', async () => {
			const content = [
				'{"timestamp":"2024-01-01T12:00:00Z","message":{"usage":{"input_tokens":100,"output_tokens":50}},"costUSD":0.01}',
				'invalid json line',
				'{"timestamp":"2024-01-01T14:00:00Z","message":{"usage":{"input_tokens":200,"output_tokens":100}},"costUSD":0.02}',
				'{ broken json',
				'{"timestamp":"2024-01-01T18:00:00Z","message":{"usage":{"input_tokens":300,"output_tokens":150}},"costUSD":0.03}',
			].join('\n');

			await using fixture = await createFixture({
				projects: { p1: { s1: { 'f.jsonl': content } } },
			});

			const expected = await loadDailyUsageData({ claudePath: fixture.path });
			const rs = runRs(['daily', '--order', 'desc'], fixture.path) as { daily: unknown[] };

			expect(expected).toHaveLength(1);
			expect(expected[0]?.inputTokens).toBe(600);
			expect(rs.daily).toHaveLength(1);
			matchSubset(rs.daily[0], { inputTokens: 600, totalCost: 0.06 });
		});

		it('model breakdowns: with model name', async () => {
			const row = {
				timestamp: '2024-01-01T12:00:00Z',
				message: {
					usage: { input_tokens: 100, output_tokens: 50 },
					model: 'claude-sonnet-4-20250514',
				},
				costUSD: 0.01,
			};

			await using fixture = await createFixture({
				projects: { p1: { s1: { 'f.jsonl': JSON.stringify(row) } } },
			});

			const expected = await loadDailyUsageData({ claudePath: fixture.path });
			const rs = runRs(['daily', '--order', 'desc'], fixture.path) as { daily: unknown[] };

			expect(expected[0]?.modelsUsed).toEqual(['claude-sonnet-4-20250514']);
			matchSubset(rs.daily[0], {
				modelsUsed: ['claude-sonnet-4-20250514'],
				modelBreakdowns: [
					{
						modelName: 'claude-sonnet-4-20250514',
						inputTokens: 100,
						outputTokens: 50,
						cacheCreationTokens: 0,
						cacheReadTokens: 0,
						cost: 0.01,
					},
				],
			});
		});
	});

	// ─── monthly ─────────────────────────────────────────────────────────────────

	describe('parity: loadMonthlyUsageData vs ccusage-rs monthly', () => {
		it('empty data returns empty array', async () => {
			await using fixture = await createFixture({ projects: {} });

			const expected = await loadMonthlyUsageData({ claudePath: fixture.path });
			const rs = runRs(['monthly', '--order', 'desc'], fixture.path) as { monthly: unknown[] };

			expect(expected).toEqual([]);
			expect(rs.monthly).toEqual([]);
		});

		it('aggregates by month correctly', async () => {
			const rows = [
				{
					timestamp: '2024-01-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-01-15T12:00:00Z',
					message: { usage: { input_tokens: 200, output_tokens: 100 } },
					costUSD: 0.02,
				},
				{
					timestamp: '2024-02-01T12:00:00Z',
					message: { usage: { input_tokens: 150, output_tokens: 75 } },
					costUSD: 0.015,
				},
			];

			await using fixture = await createFixture({
				projects: { p1: { s1: { 'f.jsonl': rows.map((d) => JSON.stringify(d)).join('\n') } } },
			});

			const expected = await loadMonthlyUsageData({ claudePath: fixture.path });
			const rs = runRs(['monthly', '--order', 'desc'], fixture.path) as { monthly: unknown[] };

			expect(expected).toHaveLength(2);
			expect(expected[0]?.month).toBe('2024-02');
			expect(expected[1]?.month).toBe('2024-01');

			expect(rs.monthly).toHaveLength(2);
			matchSubset(rs.monthly[0], { month: '2024-02', inputTokens: 150, totalCost: 0.015 });
			matchSubset(rs.monthly[1], { month: '2024-01', inputTokens: 300, totalCost: 0.03 });
		});

		it('sorts descending by default', async () => {
			const rows = [
				{
					timestamp: '2024-01-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-03-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-02-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2023-12-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
			];

			await using fixture = await createFixture({
				projects: { p1: { s1: { 'f.jsonl': rows.map((d) => JSON.stringify(d)).join('\n') } } },
			});

			const expected = await loadMonthlyUsageData({ claudePath: fixture.path });
			const rs = runRs(['monthly', '--order', 'desc'], fixture.path) as { monthly: unknown[] };

			const eMonths = expected.map((r) => r.month);
			const rsMonths = (rs.monthly as { month: string }[]).map((r) => r.month);

			expect(eMonths).toEqual(['2024-03', '2024-02', '2024-01', '2023-12']);
			expect(rsMonths).toEqual(eMonths);
		});

		it('sorts ascending when order=asc', async () => {
			const rows = [
				{
					timestamp: '2024-01-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2023-12-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-02-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2023-11-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
			];

			await using fixture = await createFixture({
				projects: { p1: { s1: { 'f.jsonl': rows.map((d) => JSON.stringify(d)).join('\n') } } },
			});

			const expected = await loadMonthlyUsageData({ claudePath: fixture.path, order: 'asc' });
			const rs = runRs(['monthly', '--order', 'asc'], fixture.path) as { monthly: unknown[] };

			const eMonths = expected.map((r) => r.month);
			const rsMonths = (rs.monthly as { month: string }[]).map((r) => r.month);
			expect(eMonths).toEqual(['2023-11', '2023-12', '2024-01', '2024-02']);
			expect(rsMonths).toEqual(eMonths);
		});

		it('respects date filters', async () => {
			const rows = [
				{
					timestamp: '2024-01-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-02-15T12:00:00Z',
					message: { usage: { input_tokens: 200, output_tokens: 100 } },
					costUSD: 0.02,
				},
				{
					timestamp: '2024-03-01T12:00:00Z',
					message: { usage: { input_tokens: 150, output_tokens: 75 } },
					costUSD: 0.015,
				},
			];

			await using fixture = await createFixture({
				projects: { p1: { s1: { 'f.jsonl': rows.map((d) => JSON.stringify(d)).join('\n') } } },
			});

			const expected = await loadMonthlyUsageData({
				claudePath: fixture.path,
				since: '20240110',
				until: '20240225',
			});
			const rs = runRs(
				['monthly', '--order', 'desc', '--since', '20240110', '--until', '20240225'],
				fixture.path,
			) as { monthly: unknown[] };

			expect(expected).toHaveLength(1);
			expect(expected[0]?.month).toBe('2024-02');
			expect(rs.monthly).toHaveLength(1);
			matchSubset(rs.monthly[0], { month: '2024-02', inputTokens: 200 });
		});

		it('handles cache tokens', async () => {
			const rows = [
				{
					timestamp: '2024-01-01T12:00:00Z',
					message: {
						usage: {
							input_tokens: 100,
							output_tokens: 50,
							cache_creation_input_tokens: 25,
							cache_read_input_tokens: 10,
						},
					},
					costUSD: 0.01,
				},
				{
					timestamp: '2024-01-15T12:00:00Z',
					message: {
						usage: {
							input_tokens: 200,
							output_tokens: 100,
							cache_creation_input_tokens: 50,
							cache_read_input_tokens: 20,
						},
					},
					costUSD: 0.02,
				},
			];

			await using fixture = await createFixture({
				projects: { p1: { s1: { 'f.jsonl': rows.map((d) => JSON.stringify(d)).join('\n') } } },
			});

			const expected = await loadMonthlyUsageData({ claudePath: fixture.path });
			const rs = runRs(['monthly', '--order', 'desc'], fixture.path) as { monthly: unknown[] };

			expect(expected[0]?.cacheCreationTokens).toBe(75);
			expect(expected[0]?.cacheReadTokens).toBe(30);
			matchSubset(rs.monthly[0], { cacheCreationTokens: 75, cacheReadTokens: 30 });
		});
	});

	// ─── weekly ──────────────────────────────────────────────────────────────────

	describe('parity: loadWeeklyUsageData vs ccusage-rs weekly', () => {
		it('empty data returns empty array', async () => {
			await using fixture = await createFixture({ projects: {} });

			const expected = await loadWeeklyUsageData({ claudePath: fixture.path });
			const rs = runRs(['weekly', '--order', 'desc'], fixture.path) as { weekly: unknown[] };

			expect(expected).toEqual([]);
			expect(rs.weekly).toEqual([]);
		});

		it('aggregates by week correctly', async () => {
			const rows = [
				{
					timestamp: '2024-01-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-01-02T12:00:00Z',
					message: { usage: { input_tokens: 200, output_tokens: 100 } },
					costUSD: 0.02,
				},
				{
					timestamp: '2024-01-15T12:00:00Z',
					message: { usage: { input_tokens: 150, output_tokens: 75 } },
					costUSD: 0.015,
				},
			];

			await using fixture = await createFixture({
				projects: { p1: { s1: { 'f.jsonl': rows.map((d) => JSON.stringify(d)).join('\n') } } },
			});

			const expected = await loadWeeklyUsageData({ claudePath: fixture.path });
			const rs = runRs(['weekly', '--order', 'desc'], fixture.path) as { weekly: unknown[] };

			expect(expected).toHaveLength(2);
			expect(expected[0]?.week).toBe('2024-01-14');
			expect(expected[1]?.week).toBe('2023-12-31');

			expect(rs.weekly).toHaveLength(2);
			matchSubset(rs.weekly[0], { week: '2024-01-14', inputTokens: 150 });
			matchSubset(rs.weekly[1], { week: '2023-12-31', inputTokens: 300 });
		});

		it('sorts by week descending by default', async () => {
			const rows = [
				{
					timestamp: '2024-01-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-01-08T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-01-15T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-01-22T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
			];

			await using fixture = await createFixture({
				projects: { p1: { s1: { 'f.jsonl': rows.map((d) => JSON.stringify(d)).join('\n') } } },
			});

			const expected = await loadWeeklyUsageData({ claudePath: fixture.path });
			const rs = runRs(['weekly', '--order', 'desc'], fixture.path) as { weekly: unknown[] };

			const eWeeks = expected.map((r) => r.week);
			const rsWeeks = (rs.weekly as { week: string }[]).map((r) => r.week);

			expect(eWeeks).toEqual(['2024-01-21', '2024-01-14', '2024-01-07', '2023-12-31']);
			expect(rsWeeks).toEqual(eWeeks);
		});

		it('sorts ascending when order=asc', async () => {
			const rows = [
				{
					timestamp: '2024-01-01T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-01-08T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-01-15T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-01-22T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
			];

			await using fixture = await createFixture({
				projects: { p1: { s1: { 'f.jsonl': rows.map((d) => JSON.stringify(d)).join('\n') } } },
			});

			const expected = await loadWeeklyUsageData({ claudePath: fixture.path, order: 'asc' });
			const rs = runRs(['weekly', '--order', 'asc'], fixture.path) as { weekly: unknown[] };

			const eWeeks = expected.map((r) => r.week);
			const rsWeeks = (rs.weekly as { week: string }[]).map((r) => r.week);
			expect(eWeeks).toEqual(['2023-12-31', '2024-01-07', '2024-01-14', '2024-01-21']);
			expect(rsWeeks).toEqual(eWeeks);
		});

		it('respects date filters', async () => {
			const rows = [
				{
					timestamp: '2024-01-02T12:00:00Z',
					message: { usage: { input_tokens: 100, output_tokens: 50 } },
					costUSD: 0.01,
				},
				{
					timestamp: '2024-02-06T12:00:00Z',
					message: { usage: { input_tokens: 200, output_tokens: 100 } },
					costUSD: 0.02,
				},
				{
					timestamp: '2024-03-05T12:00:00Z',
					message: { usage: { input_tokens: 150, output_tokens: 75 } },
					costUSD: 0.015,
				},
			];

			await using fixture = await createFixture({
				projects: { p1: { s1: { 'f.jsonl': rows.map((d) => JSON.stringify(d)).join('\n') } } },
			});

			const expected = await loadWeeklyUsageData({
				claudePath: fixture.path,
				since: '20240110',
				until: '20240225',
			});
			const rs = runRs(
				['weekly', '--order', 'desc', '--since', '20240110', '--until', '20240225'],
				fixture.path,
			) as { weekly: unknown[] };

			expect(expected).toHaveLength(1);
			expect(expected[0]?.week).toBe('2024-02-04');
			expect(rs.weekly).toHaveLength(1);
			matchSubset(rs.weekly[0], { week: '2024-02-04', inputTokens: 200 });
		});
	});

	// ─── session ─────────────────────────────────────────────────────────────────

	describe('parity: loadSessionData vs ccusage-rs session', () => {
		it('empty data returns empty array', async () => {
			await using fixture = await createFixture({ projects: {} });

			const expected = await loadSessionData({ claudePath: fixture.path });
			// ccusage-rs CLI (like upstream CLI): empty session prints `[]` (bare array, not wrapped)
			const rs = runRs(['session', '--order', 'desc'], fixture.path) as unknown[];

			expect(expected).toEqual([]);
			expect(rs).toEqual([]);
		});

		it('aggregates session usage data', async () => {
			const rows = [
				{
					timestamp: '2024-01-01T12:00:00Z',
					message: {
						usage: {
							input_tokens: 100,
							output_tokens: 50,
							cache_creation_input_tokens: 10,
							cache_read_input_tokens: 5,
						},
					},
					costUSD: 0.01,
				},
				{
					timestamp: '2024-01-01T13:00:00Z',
					message: {
						usage: {
							input_tokens: 200,
							output_tokens: 100,
							cache_creation_input_tokens: 20,
							cache_read_input_tokens: 10,
						},
					},
					costUSD: 0.02,
				},
			];

			await using fixture = await createFixture({
				projects: {
					project1: {
						session1: {
							'chat.jsonl': rows.map((d) => JSON.stringify(d)).join('\n'),
						},
					},
				},
			});

			const expected = await loadSessionData({ claudePath: fixture.path });
			const rs = runRs(['session', '--order', 'desc'], fixture.path) as { sessions: unknown[] };

			expect(expected).toHaveLength(1);
			expect(expected[0]?.sessionId).toBe('session1');
			expect(expected[0]?.projectPath).toBe('project1');
			expect(expected[0]?.inputTokens).toBe(300);
			expect(expected[0]?.outputTokens).toBe(150);
			expect(expected[0]?.cacheCreationTokens).toBe(30);
			expect(expected[0]?.cacheReadTokens).toBe(15);
			expect(expected[0]?.totalCost).toBe(0.03);
			expect(expected[0]?.lastActivity).toBe('2024-01-01');

			expect(rs.sessions).toHaveLength(1);
			matchSubset(rs.sessions[0], {
				sessionId: 'session1',
				projectPath: 'project1',
				inputTokens: 300,
				outputTokens: 150,
				cacheCreationTokens: 30,
				cacheReadTokens: 15,
				totalCost: 0.03,
				lastActivity: '2024-01-01',
			});
		});

		it('sorts sessions by last activity descending by default', async () => {
			const sessions = [
				{ id: 'session1', ts: '2024-01-15T12:00:00Z', cost: 0.01 },
				{ id: 'session2', ts: '2024-01-01T12:00:00Z', cost: 0.01 },
				{ id: 'session3', ts: '2024-01-31T12:00:00Z', cost: 0.01 },
			];

			await using fixture = await createFixture({
				projects: {
					project1: Object.fromEntries(
						sessions.map((s) => [
							s.id,
							{
								'chat.jsonl': JSON.stringify({
									timestamp: s.ts,
									message: { usage: { input_tokens: 100, output_tokens: 50 } },
									costUSD: s.cost,
								}),
							},
						]),
					),
				},
			});

			const expected = await loadSessionData({ claudePath: fixture.path });
			const rs = runRs(['session', '--order', 'desc'], fixture.path) as { sessions: unknown[] };

			const eIds = expected.map((s) => s.sessionId);
			const rsIds = (rs.sessions as { sessionId: string }[]).map((s) => s.sessionId);

			expect(eIds).toEqual(['session3', 'session1', 'session2']);
			expect(rsIds).toEqual(eIds);
		});

		it('CLI parity: --order asc is silently ignored, output stays desc', async () => {
			// Upstream `commands/session.ts` strips the `order` arg before delegating
			// to loadSessionData, so the CLI always outputs desc regardless of --order.
			// The library function loadSessionData({order:'asc'}) DOES honor asc, but
			// ccusage-rs is a CLI drop-in — its job is to match upstream's *CLI*.
			const sessions = [
				{ id: 'session1', ts: '2024-01-15T12:00:00Z', cost: 0.01 },
				{ id: 'session2', ts: '2024-01-01T12:00:00Z', cost: 0.01 },
				{ id: 'session3', ts: '2024-01-31T12:00:00Z', cost: 0.01 },
			];

			await using fixture = await createFixture({
				projects: {
					project1: Object.fromEntries(
						sessions.map((s) => [
							s.id,
							{
								'chat.jsonl': JSON.stringify({
									timestamp: s.ts,
									message: { usage: { input_tokens: 100, output_tokens: 50 } },
									costUSD: s.cost,
								}),
							},
						]),
					),
				},
			});

			// The library function with order:'asc' would give us [session2, session1, session3]
			// but the CLI strips the flag, so we compare against loadSessionData WITHOUT order
			// (which defaults to desc) — that's what the CLI actually does.
			const expected = await loadSessionData({ claudePath: fixture.path });
			const rs = runRs(['session', '--order', 'asc'], fixture.path) as { sessions: unknown[] };

			const eIds = expected.map((s) => s.sessionId);
			const rsIds = (rs.sessions as { sessionId: string }[]).map((s) => s.sessionId);

			expect(eIds).toEqual(['session3', 'session1', 'session2']);
			expect(rsIds).toEqual(eIds);
		});

		it('filters sessions by date range', async () => {
			const sessions = [
				{ id: 'session1', ts: '2024-01-01T12:00:00Z' },
				{ id: 'session2', ts: '2024-01-15T12:00:00Z' },
				{ id: 'session3', ts: '2024-01-31T12:00:00Z' },
			];

			await using fixture = await createFixture({
				projects: {
					project1: Object.fromEntries(
						sessions.map((s) => [
							s.id,
							{
								'chat.jsonl': JSON.stringify({
									timestamp: s.ts,
									message: { usage: { input_tokens: 100, output_tokens: 50 } },
									costUSD: 0.01,
								}),
							},
						]),
					),
				},
			});

			const expected = await loadSessionData({
				claudePath: fixture.path,
				since: '20240110',
				until: '20240125',
			});
			const rs = runRs(
				['session', '--order', 'desc', '--since', '20240110', '--until', '20240125'],
				fixture.path,
			) as { sessions: unknown[] };

			expect(expected).toHaveLength(1);
			expect(expected[0]?.lastActivity).toBe('2024-01-15');
			expect(rs.sessions).toHaveLength(1);
			matchSubset(rs.sessions[0], { lastActivity: '2024-01-15' });
		});
	});

	// ─── session --id ────────────────────────────────────────────────────────────

	describe('parity: loadSessionUsageById vs ccusage-rs session --id', () => {
		it('returns null for non-existent session', async () => {
			await using fixture = await createFixture({
				'.claude': {
					projects: {
						'test-project': {
							'other-session.jsonl': JSON.stringify({
								timestamp: '2024-01-01T00:00:00Z',
								sessionId: 'other-session',
								message: {
									usage: { input_tokens: 100, output_tokens: 50 },
									model: 'claude-sonnet-4-20250514',
								},
								costUSD: 0.5,
							}),
						},
					},
				},
			});

			// upstream uses CLAUDE_CONFIG_DIR pointing at .claude subdir
			vi.stubEnv('CLAUDE_CONFIG_DIR', fixture.getPath('.claude'));
			const upstreamResult = await loadSessionUsageById('non-existent', { mode: 'display' });
			vi.unstubAllEnvs();

			// ccusage-rs: fixture.getPath('.claude') contains projects/
			const rsRaw = spawnSync(
				RUST_BIN,
				['session', '--id', 'non-existent', '--json', '--offline'],
				{
					env: { ...process.env, TZ: 'UTC', CLAUDE_CONFIG_DIR: fixture.getPath('.claude') },
					encoding: 'utf8',
					timeout: 15_000,
				},
			);
			const rsResult = JSON.parse(rsRaw.stdout);

			expect(upstreamResult).toBeNull();
			expect(rsResult).toBeNull();
		});

		it('loads session data for existing session', async () => {
			await using fixture = await createFixture({
				'.claude': {
					projects: {
						'test-project': {
							'session-123.jsonl': [
								JSON.stringify({
									timestamp: '2024-01-01T00:00:00Z',
									sessionId: 'session-123',
									message: {
										usage: {
											input_tokens: 100,
											output_tokens: 50,
											cache_creation_input_tokens: 10,
											cache_read_input_tokens: 20,
										},
										model: 'claude-sonnet-4-20250514',
									},
									costUSD: 0.5,
								}),
								JSON.stringify({
									timestamp: '2024-01-01T01:00:00Z',
									sessionId: 'session-123',
									message: {
										usage: {
											input_tokens: 200,
											output_tokens: 100,
											cache_creation_input_tokens: 20,
											cache_read_input_tokens: 40,
										},
										model: 'claude-sonnet-4-20250514',
									},
									costUSD: 1.0,
								}),
							].join('\n'),
						},
					},
				},
			});

			vi.stubEnv('CLAUDE_CONFIG_DIR', fixture.getPath('.claude'));
			const upstreamResult = await loadSessionUsageById('session-123', { mode: 'display' });
			vi.unstubAllEnvs();

			const rsRaw = spawnSync(
				RUST_BIN,
				['session', '--id', 'session-123', '--json', '--offline'],
				{
					env: { ...process.env, TZ: 'UTC', CLAUDE_CONFIG_DIR: fixture.getPath('.claude') },
					encoding: 'utf8',
					timeout: 15_000,
				},
			);
			const rsResult = JSON.parse(rsRaw.stdout) as {
				sessionId: string;
				totalCost: number;
				totalTokens: number;
				entries: unknown[];
			};

			expect(upstreamResult).not.toBeNull();
			expect(upstreamResult?.totalCost).toBe(1.5);
			expect(upstreamResult?.entries).toHaveLength(2);

			// ccusage-rs returns {sessionId, totalCost, totalTokens, entries}
			// upstream returns {totalCost, entries} — compare shared fields
			expect(rsResult.sessionId).toBe('session-123');
			expect(rsResult.totalCost).toBe(upstreamResult?.totalCost);
			expect(rsResult.entries).toHaveLength(upstreamResult?.entries.length ?? 0);
		});
	});

	// ─── blocks ──────────────────────────────────────────────────────────────────

	describe('parity: loadSessionBlockData vs ccusage-rs blocks', () => {
		// Pin "now" for both upstream (vi.useFakeTimers) and ccusage-rs (--now)
		const PINNED_NOW = '2099-01-01T00:00:00Z';

		it('empty data returns empty array', async () => {
			await using fixture = await createFixture({ projects: {} });

			const expected = await loadSessionBlockData({ claudePath: fixture.path });
			const rs = runRs(['blocks', '--order', 'asc'], fixture.path, PINNED_NOW) as {
				blocks: unknown[];
			};

			expect(expected).toEqual([]);
			expect(rs.blocks).toEqual([]);
		});

		it('correctly splits entries into blocks', async () => {
			const now = new Date('2024-01-01T10:00:00Z');
			const laterTime = new Date(now.getTime() + 1 * 60 * 60 * 1000); // +1h (same block)
			const muchLaterTime = new Date(now.getTime() + 6 * 60 * 60 * 1000); // +6h (next block)

			await using fixture = await createFixture({
				projects: {
					project1: {
						session1: {
							'conv.jsonl': [
								{
									timestamp: now.toISOString(),
									message: {
										id: 'msg1',
										usage: { input_tokens: 1000, output_tokens: 500 },
										model: createModelName('claude-sonnet-4-20250514'),
									},
									requestId: 'req1',
									costUSD: 0.01,
									version: createVersion('1.0.0'),
								},
								{
									timestamp: laterTime.toISOString(),
									message: {
										id: 'msg2',
										usage: { input_tokens: 2000, output_tokens: 1000 },
										model: createModelName('claude-sonnet-4-20250514'),
									},
									requestId: 'req2',
									costUSD: 0.02,
									version: createVersion('1.0.0'),
								},
								{
									timestamp: muchLaterTime.toISOString(),
									message: {
										id: 'msg3',
										usage: { input_tokens: 1500, output_tokens: 750 },
										model: createModelName('claude-sonnet-4-20250514'),
									},
									requestId: 'req3',
									costUSD: 0.015,
									version: createVersion('1.0.0'),
								},
							]
								.map((d) => JSON.stringify(d))
								.join('\n'),
						},
					},
				},
			});

			vi.useFakeTimers();
			vi.setSystemTime(new Date(PINNED_NOW));

			const expected = await loadSessionBlockData({ claudePath: fixture.path });

			vi.useRealTimers();

			const rs = runRs(['blocks', '--order', 'asc'], fixture.path, PINNED_NOW) as {
				blocks: { entries: number; costUSD: number }[];
			};

			expect(expected.length).toBeGreaterThan(0);
			expect(rs.blocks.length).toBeGreaterThan(0);

			// Both should have same number of blocks
			expect(rs.blocks.length).toBe(expected.length);

			// Total entries across all blocks should be 3
			const upstreamTotalEntries = expected.reduce((sum, b) => sum + b.entries.length, 0);
			const rsTotalEntries = rs.blocks.reduce((sum, b) => sum + b.entries, 0);
			expect(upstreamTotalEntries).toBe(3);
			expect(rsTotalEntries).toBe(3);
		});

		it('deduplicates entries correctly', async () => {
			const now = new Date('2024-01-01T10:00:00Z');

			const entry = {
				timestamp: now.toISOString(),
				message: {
					id: 'msg1',
					usage: { input_tokens: 1000, output_tokens: 500 },
					model: createModelName('claude-sonnet-4-20250514'),
				},
				requestId: 'req1',
				costUSD: 0.01,
				version: createVersion('1.0.0'),
			};

			await using fixture = await createFixture({
				projects: {
					project1: {
						session1: {
							'conv.jsonl': [
								JSON.stringify(entry),
								JSON.stringify(entry), // duplicate
							].join('\n'),
						},
					},
				},
			});

			vi.useFakeTimers();
			vi.setSystemTime(new Date(PINNED_NOW));
			const expected = await loadSessionBlockData({ claudePath: fixture.path });
			vi.useRealTimers();

			const rs = runRs(['blocks', '--order', 'asc'], fixture.path, PINNED_NOW) as {
				blocks: { entries: number }[];
			};

			expect(expected).toHaveLength(1);
			expect(expected[0]?.entries).toHaveLength(1);

			expect(rs.blocks).toHaveLength(1);
			expect(rs.blocks[0]?.entries).toBe(1);
		});

		it('filters by date range', async () => {
			const date1 = new Date('2024-01-01T10:00:00Z');
			const date2 = new Date('2024-01-02T10:00:00Z');
			const date3 = new Date('2024-01-03T10:00:00Z');

			const mkEntry = (date: Date, id: string, cost: number) => ({
				timestamp: date.toISOString(),
				message: {
					id,
					usage: { input_tokens: 1000, output_tokens: 500 },
					model: createModelName('claude-sonnet-4-20250514'),
				},
				requestId: `req-${id}`,
				costUSD: cost,
				version: createVersion('1.0.0'),
			});

			await using fixture = await createFixture({
				projects: {
					project1: {
						session1: {
							'conv.jsonl': [
								JSON.stringify(mkEntry(date1, 'msg1', 0.01)),
								JSON.stringify(mkEntry(date2, 'msg2', 0.02)),
								JSON.stringify(mkEntry(date3, 'msg3', 0.015)),
							].join('\n'),
						},
					},
				},
			});

			vi.useFakeTimers();
			vi.setSystemTime(new Date(PINNED_NOW));
			const sincResult = await loadSessionBlockData({
				claudePath: fixture.path,
				since: '20240102',
			});
			vi.useRealTimers();

			const rs = runRs(
				['blocks', '--order', 'asc', '--since', '20240102'],
				fixture.path,
				PINNED_NOW,
			) as { blocks: { startTime: string }[] };

			expect(sincResult.length).toBeGreaterThan(0);
			expect(rs.blocks.length).toBeGreaterThan(0);
			expect(rs.blocks.length).toBe(sincResult.length);

			// All returned blocks should be on or after date2
			for (const block of rs.blocks) {
				expect(new Date(block.startTime) >= date2).toBe(true);
			}
		});
	});

	// ─── --project filter ─────────────────────────────────────────────────────────

	describe('parity: --project filter', () => {
		it('daily with project filter', async () => {
			const row = {
				timestamp: '2024-01-01T12:00:00Z',
				message: { usage: { input_tokens: 100, output_tokens: 50 } },
				costUSD: 0.01,
			};

			await using fixture = await createFixture({
				projects: {
					'target-project': {
						session1: { 'f.jsonl': JSON.stringify({ ...row, costUSD: 0.01 }) },
					},
					'other-project': {
						session2: { 'f.jsonl': JSON.stringify({ ...row, costUSD: 0.99 }) },
					},
				},
			});

			const expected = await loadDailyUsageData({
				claudePath: fixture.path,
				project: 'target-project',
			});
			const rs = runRs(
				['daily', '--order', 'desc', '--project', 'target-project'],
				fixture.path,
			) as { daily: unknown[] };

			expect(expected).toHaveLength(1);
			expect(expected[0]?.totalCost).toBe(0.01);
			expect(rs.daily).toHaveLength(1);
			matchSubset(rs.daily[0], { totalCost: 0.01 });
		});
	});
}
