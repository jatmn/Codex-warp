import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';
import Ajv from 'ajv';
import addFormats from 'ajv-formats';

const require = createRequire(import.meta.url);
const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../..');

const releasePlease = require('release-please');
assert.equal(
  releasePlease.VERSION,
  '17.6.0',
  'the harness must run the release-please version embedded by the pinned action'
);

// These three seams are intentionally pinned compatibility dependencies. The
// public index does not expose the parser, Version, or default notes builder in
// release-please 17.6.0; an upgrade must fail until these imports are reviewed.
const {parseConventionalCommits} = require('release-please/build/src/commit.js');
const {Version} = require('release-please/build/src/version.js');
const {DefaultVersioningStrategy} = require(
  'release-please/build/src/versioning-strategies/default.js'
);
const {DefaultChangelogNotes} = require(
  'release-please/build/src/changelog-notes/default.js'
);

const config = JSON.parse(fs.readFileSync(path.join(root, 'release-please-config.json')));
const vendoredSchema = JSON.parse(
  fs.readFileSync(path.join(here, 'config.schema.json'))
);
assert.deepEqual(
  vendoredSchema,
  releasePlease.configSchema,
  'vendored config schema drifted from release-please 17.6.0'
);
const ajv = new Ajv({allErrors: true, strict: false});
addFormats(ajv);
assert.equal(
  ajv.validate(vendoredSchema, config),
  true,
  ajv.errorsText(ajv.errors, {separator: '\n'})
);

const fixture = JSON.parse(
  fs.readFileSync(path.join(here, 'fixtures/version-policy.json'))
);
const changelogSections = config['changelog-sections'];
const versioning = new DefaultVersioningStrategy({bumpMinorPreMajor: true});
const notesBuilder = new DefaultChangelogNotes();

for (const [index, testCase] of fixture.cases.entries()) {
  const commits = parseConventionalCommits([
    {
      sha: String(index + 1).padStart(40, '0'),
      message: testCase.message,
    },
  ]);
  assert.ok(commits.length > 0, `${testCase.name}: parser returned no commits`);

  const notes = await notesBuilder.buildNotes(commits, {
    owner: 'jatmn',
    repository: 'Codex-warp',
    version: testCase.version ?? fixture.baseVersion,
    currentTag: `v${testCase.version ?? fixture.baseVersion}`,
    targetBranch: 'main',
    changelogSections,
  });
  const visible = notes.split('\n').length > 1;

  if (testCase.version === null) {
    assert.equal(visible, false, `${testCase.name}: expected no release notes`);
    continue;
  }

  assert.equal(visible, true, `${testCase.name}: expected visible release notes`);
  const next = versioning.bump(Version.parse(fixture.baseVersion), commits);
  assert.equal(next.toString(), testCase.version, `${testCase.name}: version`);
  assert.match(
    notes,
    new RegExp(`^### ${testCase.section.replace(/[.*+?^${}()|[\\]\\]/g, '\\$&')}$`, 'm'),
    `${testCase.name}: changelog section`
  );
}

console.log(`release-please-policy-harness: ${fixture.cases.length} cases ok`);
