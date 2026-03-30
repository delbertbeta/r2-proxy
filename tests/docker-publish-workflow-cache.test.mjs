import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

test("docker publish workflow uses gha layer cache", () => {
  const workflow = readFileSync(new URL("../.github/workflows/docker-publish.yml", import.meta.url), "utf8");

  assert.match(workflow, /cache-from:\s*type=gha/);
  assert.match(workflow, /cache-to:\s*type=gha,mode=max/);
});
