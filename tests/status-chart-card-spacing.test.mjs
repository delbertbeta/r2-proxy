import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

test("chart cards keep an 18px top margin", () => {
  const css = readFileSync(new URL("../status/app.css", import.meta.url), "utf8");
  const chartCardBlock = css.match(/\.chart-card\s*\{[^}]*\}/);

  assert.ok(chartCardBlock, "expected to find the .chart-card CSS block");
  assert.match(chartCardBlock[0], /margin-top:\s*18px;/);
});
