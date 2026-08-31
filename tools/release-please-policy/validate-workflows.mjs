import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import YAML from 'yaml';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../..');
const read = relative => fs.readFileSync(path.join(root, relative), 'utf8');
const parse = relative => YAML.parse(read(relative), {uniqueKeys: true});
const tooling = JSON.parse(read('tools/release-tooling.json'));
const contract = JSON.parse(read('tools/release-contract.json'));
const workflowFiles = fs.readdirSync(path.join(root, '.github/workflows'))
  .filter(file => /\.ya?ml$/.test(file))
  .map(file => `.github/workflows/${file}`);

function visit(value, callback) {
  if (Array.isArray(value)) {
    for (const item of value) visit(item, callback);
  } else if (value && typeof value === 'object') {
    callback(value);
    for (const item of Object.values(value)) visit(item, callback);
  }
}

const allowedActions = new Map([
  ['actions/checkout', tooling.actions.checkout],
  ['actions/setup-node', tooling.actions.setupNode],
  ['actions/upload-artifact', tooling.actions.uploadArtifact],
  ['actions/download-artifact', tooling.actions.downloadArtifact],
  ['actions/attest', tooling.actions.attest],
  ['actions/attest-build-provenance', tooling.actions.attestBuildProvenance],
  ['actions/create-github-app-token', tooling.actions.createGitHubAppToken],
  ['googleapis/release-please-action', tooling.releasePleaseAction.commit],
  ['dtolnay/rust-toolchain', tooling.actions.rustToolchain],
  ['Swatinem/rust-cache', tooling.actions.rustCache],
  ['taiki-e/install-action', tooling.actions.installAction],
  ['rustsec/audit-check', tooling.actions.auditCheck],
  ['crate-ci/typos', tooling.actions.typos],
  ['ilammy/setup-nasm', tooling.actions.setupNasm],
]);

for (const file of workflowFiles) {
  const source = read(file);
  assert.ok(!source.includes('pull_request_target:'), `${file} must not use pull_request_target`);
  assert.ok(!/curl[^\n]*\|\s*(?:bash|sh)/.test(source), `${file} must not execute a remote installer`);
  assert.ok(!source.includes('--jq --arg'), `${file} passes unsupported jq arguments to gh --jq`);
  for (const match of source.matchAll(/\bnpm ci[^\n]*/g)) {
    assert.ok(match[0].includes('--omit=dev'), `${file} installs dev-only release tooling`);
    assert.ok(match[0].includes('--ignore-scripts'), `${file} enables dependency lifecycle scripts`);
  }
  const workflow = parse(file);
  assert.deepEqual(workflow.permissions, {contents: 'read'}, `${file} must default to read-only contents`);
  for (const [jobName, job] of Object.entries(workflow.jobs)) {
    const jobText = JSON.stringify(job);
    if (jobText.includes('secrets.RELEASE_APP_PRIVATE_KEY') || jobText.includes('actions/create-github-app-token')) {
      assert.equal(job.environment, 'release-automation', `${file}:${jobName} reads the App key outside the protected environment`);
    }
    for (const [index, step] of (job.steps || []).entries()) {
      if (typeof step.run !== 'string') continue;
      const bashStep = step.shell === 'bash' || (!step.shell && String(job['runs-on']).startsWith('ubuntu'));
      if (!bashStep) continue;
      const script = step.run.replace(/\$\{\{[^}]*\}\}/g, 'GITHUB_EXPRESSION');
      const syntax = spawnSync('bash', ['-n', '-c', script], {encoding: 'utf8', timeout: 2000});
      assert.equal(syntax.status, 0, `${file}:${jobName}:step-${index + 1} has invalid Bash:\n${syntax.stderr}`);
    }
  }
  visit(workflow, object => {
    if (typeof object.uses !== 'string' || object.uses.startsWith('./')) return;
    const match = object.uses.match(/^([^@]+)@([0-9a-f]{40})$/);
    assert.ok(match, `${file} has a non-immutable Action reference: ${object.uses}`);
    assert.equal(allowedActions.get(match[1]), match[2], `${file} has an unreviewed Action pin: ${object.uses}`);
  });
}

const releasePlease = parse('.github/workflows/release-please.yml');
assert.deepEqual(releasePlease.on.push.branches, ['main']);
assert.ok(Object.hasOwn(releasePlease.on, 'workflow_dispatch'));
assert.deepEqual(releasePlease.concurrency, {group: 'release-please', queue: 'max'});
assert.equal(releasePlease.jobs['release-please'].environment, 'release-automation');
assert.equal(releasePlease.jobs['release-please'].if, "needs.gate.outputs.ready == 'true'");
const rpToken = releasePlease.jobs['release-please'].steps.find(step => step.id === 'app-token');
assert.deepEqual(rpToken.with, {
  'client-id': '${{ vars.RELEASE_APP_CLIENT_ID }}',
  'private-key': '${{ secrets.RELEASE_APP_PRIVATE_KEY }}',
  owner: '${{ github.repository_owner }}',
  repositories: 'Codex-warp',
  'permission-contents': 'write',
  'permission-pull-requests': 'write',
  'permission-issues': 'write',
  'permission-workflows': 'write',
});
const rpAction = releasePlease.jobs['release-please'].steps.find(step => step.id === 'release');
assert.equal(rpAction.uses, `googleapis/release-please-action@${tooling.releasePleaseAction.commit}`);
assert.equal(rpAction.with['target-branch'], 'main');
assert.equal(rpAction.with['config-file'], 'release-please-config.json');
assert.equal(rpAction.with['manifest-file'], '.release-please-manifest.json');

const nightly = parse('.github/workflows/nightly.yml');
assert.deepEqual(nightly.concurrency, {group: 'nightly-release', queue: 'max'});
assert.deepEqual(nightly.on.schedule, [{cron: '17 3 * * *', timezone: 'America/Los_Angeles'}]);
assert.equal(nightly.on.workflow_dispatch.inputs.dry_run.default, true);
assert.equal(nightly.on.workflow_dispatch.inputs.source_sha.default, '');
const prepare = nightly.jobs.prepare.steps.find(step => step.id === 'prepare');
assert.equal(prepare.env.NIGHTLY_DRY_RUN, "${{ github.event_name != 'workflow_dispatch' || inputs.dry_run }}");
assert.equal(prepare.env.NIGHTLY_PUBLISH_ENABLED, "${{ vars.NIGHTLY_PUBLISH_ENABLED || 'false' }}");
const matrix = nightly.jobs.build.strategy.matrix.include;
assert.deepEqual(matrix.map(item => item.target).sort(), contract.targets.map(item => item.triple).sort());
assert.deepEqual(matrix.map(item => item.runner), ['ubuntu-24.04', 'macos-15', 'macos-15-intel', 'windows-2025']);
assert.equal(nightly.jobs.publish.environment, 'release-automation');
assert.ok(nightly.jobs.publish.if.includes("vars.NIGHTLY_MUTATION_READY == 'true'"));
assert.ok(nightly.jobs.publish.if.includes("needs.prepare.outputs.publish == 'true'"));
const nightlyToken = nightly.jobs.publish.steps.find(step => step.id === 'app-token');
assert.equal(nightlyToken.with['client-id'], '${{ vars.RELEASE_APP_CLIENT_ID }}');
assert.equal(nightlyToken.with['permission-contents'], 'write');
assert.equal(nightlyToken.with['permission-workflows'], 'write');

const release = parse('.github/workflows/release.yml');
const releaseSource = read('.github/workflows/release.yml');
assert.ok(
  releaseSource.includes('name: Validate and smoke-test archive') &&
    releaseSource.includes('bash scripts/check-release-contract.sh archive "target/distrib/$archive"'),
  'official release builds must validate and smoke-test every native archive before attestation'
);
assert.deepEqual(release.on.push.tags, ['v[0-9]+.[0-9]+.[0-9]+']);
assert.equal(release.concurrency.queue, 'max');
const proofPrepare = release.jobs['prepare-pr-upload-proof'];
assert.equal(proofPrepare.environment, undefined);
assert.deepEqual(proofPrepare.permissions, {contents: 'read'});
assert.ok(proofPrepare.if.includes("github.event_name == 'pull_request'"));
assert.ok(proofPrepare.if.includes('github.event.pull_request.head.repo.full_name == github.repository'));
assert.ok(proofPrepare.if.includes("fromJson(needs.plan.outputs.val).ci.github.pr_run_mode == 'upload'"));
assert.ok(proofPrepare.steps.some(step => step.run === 'bash scripts/assemble-pr-upload-proof.sh target/distrib identity.json pr-upload-proof'));
const proofAttest = release.jobs['attest-pr-upload-proof-metadata'];
assert.equal(proofAttest.environment, undefined);
assert.deepEqual(proofAttest.permissions, {attestations: 'write', contents: 'read', 'id-token': 'write'});
assert.ok(proofAttest.steps.some(step => typeof step.run === 'string' && step.run.includes('check-release-contract.sh pr-upload-proof')));
assert.equal(proofAttest.steps.find(step => step.uses?.startsWith('actions/attest-build-provenance@')).with['subject-path'],
  'pr-upload-proof/codex-warp-release-metadata.json');
assert.ok(release.jobs['publish-official-release'].if.includes("vars.OFFICIAL_RELEASES_ENABLED == 'true'"));
assert.equal(release.jobs['publish-official-release'].environment, 'release-automation');
const officialAttest = release.jobs['attest-official-metadata'];
assert.ok(officialAttest.steps.some(step => typeof step.run === 'string' && step.run.includes('check-release-contract.sh official-publication')));
const officialToken = release.jobs['publish-official-release'].steps.find(step => step.id === 'app-token');
assert.equal(officialToken.with['client-id'], '${{ vars.RELEASE_APP_CLIENT_ID }}');
assert.equal(officialToken.with['permission-contents'], 'write');
assert.equal(officialToken.with['permission-workflows'], 'write');

const officialRecovery = parse('.github/workflows/release-recovery.yml');
assert.deepEqual(Object.keys(officialRecovery.on), ['workflow_dispatch']);
assert.deepEqual(officialRecovery.concurrency, {group: 'official-release-${{ inputs.tag }}', queue: 'max'});
assert.deepEqual(officialRecovery.on.workflow_dispatch.inputs.operation.options,
  ['rebuild-draft', 'resume-upload', 'replace-unpublished-assets', 'publish-verified-draft']);
assert.equal(officialRecovery.jobs.mutate.environment, 'release-automation');
assert.ok(officialRecovery.jobs.mutate.if.includes("vars.OFFICIAL_RECOVERY_READY == 'true'"));
const officialRecoverySource = read('.github/workflows/release-recovery.yml');
assert.ok(!officialRecoverySource.includes('-f ref=refs/tags/'), 'official recovery must never create or move a tag');
assert.ok(!officialRecoverySource.includes('--method DELETE "repos/${{ github.repository }}/releases/$RELEASE_ID"'), 'official recovery must never delete a release');

const nightlyRecovery = parse('.github/workflows/nightly-recovery.yml');
assert.deepEqual(Object.keys(nightlyRecovery.on), ['workflow_dispatch']);
assert.deepEqual(nightlyRecovery.concurrency, {group: 'nightly-release', queue: 'max'});
assert.deepEqual(nightlyRecovery.on.workflow_dispatch.inputs.operation.options,
  ['resume-draft', 'replace-unpublished-draft', 'recover-orphan-tag', 'repair-branch']);
assert.ok(nightlyRecovery.jobs['mutate-release'].if.includes("vars.NIGHTLY_RECOVERY_READY == 'true'"));
assert.ok(nightlyRecovery.jobs['repair-branch'].if.includes("vars.NIGHTLY_RECOVERY_READY == 'true'"));
assert.equal(nightlyRecovery.jobs['mutate-release'].environment, 'release-automation');
assert.equal(nightlyRecovery.jobs['repair-branch'].environment, 'release-automation');
assert.deepEqual(nightlyRecovery.jobs.build.strategy.matrix.include.map(item => item.target).sort(),
  contract.targets.map(item => item.triple).sort());
const nightlyRecoverySource = read('.github/workflows/nightly-recovery.yml');
assert.ok(!nightlyRecoverySource.includes('-f ref=refs/tags/'), 'nightly recovery must never create or move a tag');

console.log('validate-workflows: pins, permissions, triggers, recovery, matrices, and mutation gates ok');
