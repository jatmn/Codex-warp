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

const {GitHub, Manifest} = releasePlease;

const notesOnlyTypes = new Set(['docs', 'build', 'ci']);

// Use a fresh, read-only Manifest for eligibility. The action constructs its
// own Manifest with all visible sections when it generates the actual notes.
async function hasReleasableChanges(manifest) {
  for (const config of Object.values(manifest.repositoryConfig)) {
    config.changelogSections = config.changelogSections.map(section =>
      notesOnlyTypes.has(section.type) ? {...section, hidden: true} : section
    );
  }
  // A merged release may not have its tag yet. Let the action finish that
  // release, without bootstrapping another PR from the previous release range.
  if ((await manifest.buildReleases()).length > 0) return false;
  return (await manifest.buildPullRequests()).length > 0;
}

async function main() {
  const policy = JSON.parse(fs.readFileSync(
    new URL('../release-automation-policy.json', import.meta.url), 'utf8'
  ));
  assert.equal(process.env.GITHUB_REPOSITORY, policy.repository);
  assert.equal(process.env.GITHUB_REF, `refs/heads/${policy.baseBranch}`);
  assert.ok(process.env.GH_TOKEN, 'read-only GitHub token is required');
  assert.ok(process.env.GITHUB_OUTPUT, 'GitHub output file is required');
  const [owner, repo] = policy.repository.split('/');
  const github = await GitHub.create({owner, repo, token: process.env.GH_TOKEN});
  const manifest = await Manifest.fromManifest(github, policy.baseBranch);
  const releasable = await hasReleasableChanges(manifest);
  fs.appendFileSync(process.env.GITHUB_OUTPUT, `releasable=${releasable}\n`);
}


if (process.argv[2] === '--release-trigger') {
  assert.equal(process.argv.length, 3, 'unexpected release-trigger arguments');
  await main();
} else {
  assert.equal(process.argv.length, 2, 'unexpected policy harness arguments');
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
      assert.equal(visible, testCase.section !== null, `${testCase.name}: notes visibility`);
      if (testCase.section !== null) {
        assert.ok(notes.includes(`### ${testCase.section}\n`), `${testCase.name}: notes section`);
      }
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



  const baseSha = 'a'.repeat(40);
  assert.equal(config['always-update'], true);

  // Replace only GitHub I/O. Version selection, commit parsing, release-PR
  // generation, pending-release discovery, and update selection use the pin.
  async function history(messages, options = {}) {
    const state = {open: [], merged: [], updates: []};
    const github = {
      repository: {owner: 'jatmn', repo: 'Codex-warp', defaultBranch: 'main'},
      async getFileJson(name) {
        if (name === 'release-please-config.json') {
          return {...structuredClone(config), ...options};
        }
        assert.equal(name, '.release-please-manifest.json');
        return {'.': fixture.baseVersion};
      },
      async getFileContentsOnBranch(name) {
        assert.equal(name, 'Cargo.toml');
        return {
          parsedContent: `[package]\nname = "codex-warp"\nversion = "${fixture.baseVersion}"\n`,
          content: '', sha: baseSha,
        };
      },
      async *releaseIterator() {
        yield {tagName: `v${fixture.baseVersion}`, sha: baseSha, notes: ''};
      },
      async *mergeCommitIterator() {
        for (const [index, message] of messages.entries()) {
          yield {sha: String(index + 1).padStart(40, '0'), message, files: ['README.md']};
        }
        yield {sha: baseSha, message: `chore(main): release ${fixture.baseVersion}`, files: []};
      },
      async *pullRequestIterator(_branch, status) {
        yield* status === 'OPEN' ? state.open : status === 'MERGED' ? state.merged : [];
      },
      async updatePullRequest(number, proposal, targetBranch) {
        assert.equal(targetBranch, 'main');
        state.updates.push({number, proposal});
        return state.open.find(pr => pr.number === number);
      },
    };
    return {github, state, manifest: await Manifest.fromManifest(github, 'main')};
  }

  for (const testCase of fixture.cases) {
    const {manifest} = await history([testCase.message]);
    assert.equal(
      await hasReleasableChanges(manifest), testCase.version !== null,
      `${testCase.name}: release eligibility`
    );
    if (testCase.version !== null) {
      const actual = await (await history([testCase.message])).manifest.buildPullRequests();
      assert.equal(actual.length, 1, `${testCase.name}: candidate count`);
      assert.equal(actual[0].version.toString(), testCase.version, `${testCase.name}: candidate version`);
    }
  }

  const maintenance = [
    'docs: clarify provider setup',
    'build(deps): update the parser',
    'ci: update the workflow',
  ];
  assert.equal(await hasReleasableChanges((await history(maintenance)).manifest), false);
  const mixed = ['fix: preserve stream usage', ...maintenance];
  assert.equal(await hasReleasableChanges((await history(mixed)).manifest), true);
  const proposals = await (await history(mixed)).manifest.buildPullRequests();
  assert.equal(proposals.length, 1);
  assert.equal(proposals[0].version.toString(), '0.0.2');
  const body = proposals[0].body.toString();
  for (const section of ['Bug Fixes', 'Documentation', 'Build System', 'Continuous Integration']) {
    assert.ok(body.includes(`### ${section}\n`), `mixed history: ${section}`);
  }
  assert.deepEqual(
    proposals[0].updates.map(update => update.path).sort(),
    ['.release-please-manifest.json', 'CHANGELOG.md', 'Cargo.lock', 'Cargo.toml']
  );
  for (const type of ['docs', 'build', 'ci']) {
    const breaking = [`${type}!: change the supported contract`];
    assert.equal(await hasReleasableChanges((await history(breaking)).manifest), true);
    const [proposal] = await (await history(breaking)).manifest.buildPullRequests();
    assert.equal(proposal.version.toString(), '0.1.0');
  }

  function existing(proposal) {
    return {
      number: 116, title: proposal.title.toString(), body: proposal.body.toString(),
      headBranchName: proposal.headRefName, baseBranchName: 'main',
      labels: ['autorelease: pending'], files: [],
    };
  }
  for (const alwaysUpdate of [false, true]) {
    const {manifest, state} = await history(mixed, {'always-update': alwaysUpdate});
    const [proposal] = await manifest.buildPullRequests();
    state.open.push(existing(proposal));
    await manifest.createPullRequests();
    assert.equal(state.updates.length, alwaysUpdate ? 1 : 0,
      'unchanged notes must still refresh an eligible release branch');
  }
  const pending = await history(mixed);
  pending.state.merged.push({...existing(proposals[0]), sha: 'b'.repeat(40)});
  assert.equal(await hasReleasableChanges(pending.manifest), false,
    'finish the merged release before planning another release PR');
  const unavailable = await history(mixed);
  unavailable.github.releaseIterator = async function* () {
    throw new Error('release history unavailable');
  };
  await assert.rejects(hasReleasableChanges(unavailable.manifest), /release history unavailable/);

  console.log('release-trigger-harness: eligibility, mixed notes, pending releases, and branch refresh ok');

}
