import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

// Use the installed compiler so this test works on every supported Node host
// without adding a separate TypeScript test runtime.
const source = readFileSync(new URL('./usageMetrics.ts', import.meta.url), 'utf8');
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ES2022 } }).outputText;
const { cacheMetricsFor, nominalCostsFor, reportedTokens, sumNominalCosts, sumKnownCost } =
    await import(`data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`);

test('cache rate uses only the samples that reported cache and input together', () => {
    const metrics = cacheMetricsFor({
        total_input_tokens: 120,
        cache_reported_requests: 1,
        cache_reported_input_tokens: 20,
        cache_reported_tokens: 10,
    }, 2);
    assert.equal(metrics.hitRate, 50);
    assert.equal(metrics.coverageRate, 50);
    assert.equal(metrics.uncachedInputTokens, 10);
});

test('an explicit zero is reported while missing and legacy fields remain unknown', () => {
    assert.equal(cacheMetricsFor({}, 1).cachedTokens, null);
    assert.equal(cacheMetricsFor({ cache_reported_requests: 1, cache_reported_input_tokens: 20, cache_reported_tokens: 0 }, 1).hitRate, 0);
    assert.equal(reportedTokens(0, 1), 0);
    assert.equal(reportedTokens(0, 0), null);
    assert.equal(reportedTokens(100), null);
});

test('nominal costs preserve independently missing input and explicit zero output', () => {
    const costs = nominalCostsFor({ total_output_tokens: 0, output_reported_requests: 1 }, { input: 2, cached: 0.2, output: 10 });
    assert.deepEqual(costs, { input: null, output: 0 });
    assert.deepEqual(nominalCostsFor({ total_output_tokens: 100 }, null), { input: null, output: null });
    assert.equal(sumKnownCost({ input: null, output: null }), null);
    assert.equal(sumKnownCost(costs), 0);
});

test('subtotal and price coverage do not invent costs for unpriced models', () => {
    assert.deepEqual(sumNominalCosts([{ input: 1, output: 2 }, { input: null, output: null }], 1), {
        input: 1, output: 2, pricedModels: 1, totalModels: 2,
    });
    assert.deepEqual(sumNominalCosts([], 0), { input: null, output: null, pricedModels: 0, totalModels: 0 });
});
