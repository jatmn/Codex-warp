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
    assert.notEqual(job.permissions?.contents, 'write', `${file}:${jobName} grants GITHUB_TOKEN contents:write`);
    const jobText = JSON.stringify(job);
    if (jobText.includes('secrets.RELEASE_APP_PRIVATE_KEY') || jobText.includes('actions/create-github-app-token')) {
      assert.equal(job.environment, 'release-automation', `${file}:${jobName} reads the App key outside the protected environment`);
    }
    for (const [index, step] of (job.steps || []).entries()) {
      if (typeof step.run !== 'string') continue;
      assert.ok(!step.run.includes('${{ inputs.'),
        `${file}:${jobName}:step-${index + 1} embeds workflow-dispatch input in shell source`);
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
const rpCheckout = releasePlease.jobs['release-please'].steps.find(step => step.uses?.startsWith('actions/checkout@'));
assert.equal(rpCheckout.with.ref, 'main');
assert.equal(rpCheckout.with['fetch-depth'], 0);
const rpRevalidate = releasePlease.jobs['release-please'].steps.find(step => step.name?.startsWith('Revalidate protected state'));
assert.ok(rpRevalidate.run.includes('git fetch --no-tags origin main') &&
  rpRevalidate.run.includes('git checkout --detach "$live_main"'),
  'Release Please must revalidate and execute from live main after its protected job starts');
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
const rpSteps = releasePlease.jobs['release-please'].steps;
const rpTokenIdx = rpSteps.findIndex(step => step.id === 'app-token');
const rpActionIdx = rpSteps.findIndex(step => step.id === 'release');
const rpPriorAfterToken = rpSteps.slice(rpTokenIdx + 1, rpActionIdx).find(step =>
  typeof step.run === 'string' && step.run.includes('check-prior-official-releases.sh'));
assert.equal(rpPriorAfterToken.env.GH_TOKEN, '${{ steps.app-token.outputs.token }}',
  'Release Please must recheck prior releases with a draft-visible App token');
assert.equal(rpPriorAfterToken.env.OFFICIAL_ALLOW_MISSING_LATEST_TAG, '1');
assert.ok(read('.github/workflows/release-please.yml').includes('OFFICIAL_ALLOW_MISSING_LATEST_TAG=1'),
  'Release Please may continue a missing-draft latest tag');
assert.ok(!read('scripts/check-release-readiness.sh').includes('OFFICIAL_ALLOW_MISSING_LATEST_TAG'),
  'a new official release PR must not merge while the latest tag is unpublished');
const rpMissingDraft = rpSteps.find(step => step.id === 'missing-draft');
assert.equal(rpMissingDraft.env.GH_TOKEN, '${{ steps.app-token.outputs.token }}',
  'missing-draft create must use the draft-visible App token');
assert.equal(rpMissingDraft.run, 'bash scripts/create-missing-official-draft.sh');
assert.ok(rpTokenIdx < rpSteps.indexOf(rpPriorAfterToken) &&
  rpSteps.indexOf(rpPriorAfterToken) < rpSteps.indexOf(rpMissingDraft) &&
  rpSteps.indexOf(rpMissingDraft) < rpActionIdx,
  'missing-draft create must run after the App-token prior-release recheck and before Release Please');
const rpAction = releasePlease.jobs['release-please'].steps.find(step => step.id === 'release');
assert.equal(rpAction.if, "steps.missing-draft.outputs.created != 'true'",
  'Release Please must not open a newer version while it just created the missing draft');
assert.equal(rpAction.uses, `googleapis/release-please-action@${tooling.releasePleaseAction.commit}`);
assert.equal(rpAction.with['target-branch'], 'main');
assert.equal(rpAction.with['config-file'], 'release-please-config.json');
assert.equal(rpAction.with['manifest-file'], '.release-please-manifest.json');
const missingDraftSource = read('scripts/create-missing-official-draft.sh');
assert.ok(missingDraftSource.includes('--method POST'),
  'missing-draft create must POST /releases');
assert.ok(!missingDraftSource.includes('/releases/tags/'),
  'missing-draft create must not look up drafts by published tag');
assert.ok(!missingDraftSource.includes('gh release'),
  'missing-draft create must not use gh release create');
assert.ok(missingDraftSource.includes('target_commitish'),
  'missing-draft create must bind the draft to the peeled tag SHA');
assert.ok(!read('.github/workflows/release-please.yml').includes('gh release create'),
  'Release Please must not use gh release create');
assert.ok(read('.github/workflows/release-please.yml').includes('releases/latest'),
  'missing-draft create must not promote the unpublished tag to Latest');

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
const nightlyReceiptDownload = nightly.jobs.publish.steps.find(step =>
  step.uses?.startsWith('actions/download-artifact@') && step.with?.name === 'nightly-tag-creation-receipt');
assert.equal(nightlyReceiptDownload.with.path, 'retained-tag-receipt');
assert.ok(nightly.jobs.publish.steps.some(step => typeof step.run === 'string' &&
  step.run.includes('cmp nightly-tag-receipt.json retained-tag-receipt/nightly-tag-receipt.json')));
const nightlyDraft = nightly.jobs.publish.steps.find(step => step.id === 'draft');
assert.ok(nightlyDraft.run.includes('/releases?per_page=100') && nightlyDraft.run.includes('length') &&
  nightlyDraft.run.includes('nightly release appeared before draft creation'),
  'nightly publication must reread exact-tag release absence immediately before draft creation');
assert.ok(nightly.jobs.publish.steps.some(step => step.name === 'Create or fast-forward nightly branch' &&
  step.run.includes('scripts/advance-nightly-branch.sh')),
  'nightly publication must use the exact API branch race protocol');
assert.deepEqual(nightly.jobs['repair-branch'].permissions,
  {actions: 'read', attestations: 'read', contents: 'read'},
  'nightly branch repair must read Actions runs to bind archive attestations');
assert.ok(nightly.jobs.publish.steps.some(step => typeof step.run === 'string' &&
  step.run.includes('GH_TOKEN="${{ github.token }}" bash scripts/verify-nightly-attestation.sh')),
  'nightly remote archive verification must use the job token, not the mutation App token');
assert.ok(read('.github/workflows/nightly.yml').includes('scripts/verify-nightly-attestation.sh'),
  'nightly publication must bind archive attestations to a trusted nightly workflow identity');
assert.ok(read('scripts/prepare-nightly.sh').includes('scripts/verify-nightly-attestation.sh'),
  'nightly prepare must bind historical archives to a trusted nightly workflow identity');
assert.ok(read('scripts/advance-nightly-branch.sh').includes('git/refs/heads/nightly'),
  'nightly branch fast-forward must use the plural refs update endpoint');
assert.ok(read('scripts/check-prior-official-releases.sh').includes('scripts/verify-official-attestation.sh'),
  'prior official release verification must bind attestations to a trusted release workflow identity');
assert.ok(!read('scripts/check-prior-official-releases.sh').includes('gh attestation verify'),
  'prior official release verification must not accept repository-scoped attestations');
assert.equal(nightly.jobs['repair-branch'].steps.find(step => step.uses?.startsWith('actions/checkout@')).with.ref, '${{ github.workflow_sha }}');
assert.ok(nightly.jobs['repair-branch'].steps.some(step => step.run?.includes('scripts/advance-nightly-branch.sh')),
  'nightly repair must use the exact API branch race protocol');
assert.ok(read('.github/workflows/nightly.yml').includes('unable to prove nightly tag absence with the mutation token'));

const release = parse('.github/workflows/release.yml');
const releaseSource = read('.github/workflows/release.yml');
assert.match(read('dist-workspace.toml'), /^pr-run-mode = "plan"$/m,
  'checked-in cargo-dist configuration must use steady-state PR planning');
assert.ok(
  releaseSource.includes('name: Validate and smoke-test archive') &&
    releaseSource.includes('bash scripts/check-release-contract.sh archive "target/distrib/$archive"'),
  'official release builds must validate and smoke-test every native archive before attestation'
);
assert.deepEqual(release.on.push.tags, ['v[0-9]+.[0-9]+.[0-9]+']);
assert.equal(release.concurrency.queue, 'max');
const proofPrepare = release.jobs['prepare-pr-upload-proof'];
assert.ok(release.jobs['build-local-artifacts'].if.includes('github.event.pull_request.head.repo.full_name == github.repository'),
  'upload-mode release builds must be limited to same-repository pull requests');
assert.equal(release.jobs['build-local-artifacts'].name, 'Build native archives',
  'skipped matrix builds must use a static check name rather than an unevaluated join');
assert.equal(proofPrepare.environment, undefined);
assert.deepEqual(proofPrepare.permissions, {contents: 'read'});
assert.ok(proofPrepare.if.includes("github.event_name == 'pull_request'"));
assert.ok(proofPrepare.if.includes('github.event.pull_request.head.repo.full_name == github.repository'));
assert.ok(proofPrepare.if.includes("fromJson(needs.plan.outputs.val).ci.github.pr_run_mode == 'upload'"));
assert.ok(proofPrepare.steps.some(step => step.run === 'bash scripts/assemble-pr-upload-proof.sh target/distrib identity.json pr-upload-proof'));
const officialPrepare = release.jobs['prepare-official-release'];
assert.ok(officialPrepare.steps.some(step => typeof step.run === 'string' &&
  step.run.includes('bash scripts/assemble-official-candidate.sh target/distrib identity.json dist-manifest.json release-assets')),
  'official prepare must assemble the eleven-file candidate from contract-named dist outputs');
assert.ok(!officialPrepare.steps.some(step => typeof step.run === 'string' &&
  step.run.includes("jq -r '.upload_files[]' dist-manifest.json")),
  'official prepare must not copy dist host upload_files as the release candidate');
const proofAttest = release.jobs['attest-pr-upload-proof-metadata'];
assert.equal(proofAttest.environment, undefined);
assert.deepEqual(proofAttest.permissions, {attestations: 'write', contents: 'read', 'id-token': 'write'});
assert.ok(proofAttest.steps.some(step => typeof step.run === 'string' && step.run.includes('check-release-contract.sh pr-upload-proof')));
assert.equal(proofAttest.steps.find(step => step.uses?.startsWith('actions/attest-build-provenance@')).with['subject-path'],
  'pr-upload-proof/codex-warp-release-metadata.json');
assert.ok(
  releaseSource.includes('bash scripts/lookup-official-draft.sh "${{ github.repository }}" "$TAG"'),
  'official prepare must look up the unpublished draft by listing releases'
);
assert.ok(
  !releaseSource.includes('/releases/tags/$TAG'),
  'official prepare must not use GET /releases/tags/{tag}, which omits drafts'
);
assert.equal(release.jobs['prepare-official-release'].environment, 'release-automation');
assert.ok(release.jobs['publish-official-release'].if.includes("vars.OFFICIAL_RELEASES_ENABLED == 'true'"));
assert.equal(release.jobs['publish-official-release'].environment, 'release-automation');
const officialAttest = release.jobs['attest-official-metadata'];
assert.ok(officialAttest.steps.some(step => typeof step.run === 'string' && step.run.includes('check-release-contract.sh official-publication')));
const officialToken = release.jobs['publish-official-release'].steps.find(step => step.id === 'app-token');
assert.equal(officialToken.with['client-id'], '${{ vars.RELEASE_APP_CLIENT_ID }}');
assert.equal(officialToken.with['permission-contents'], 'write');
assert.equal(officialToken.with['permission-workflows'], 'write');
const officialVerifyRemote = release.jobs['publish-official-release'].steps.find(
  step => step.name === 'Verify complete remote checksums');
assert.equal(officialVerifyRemote.env.GH_TOKEN, '${{ steps.app-token.outputs.token }}',
  'official publish must verify unpublished draft assets with the App token');
assert.notEqual(officialVerifyRemote.env.GH_TOKEN, '${{ github.token }}',
  'contents:read GITHUB_TOKEN cannot download unpublished draft assets');

const officialRecovery = parse('.github/workflows/release-recovery.yml');
assert.deepEqual(Object.keys(officialRecovery.on), ['workflow_dispatch']);
assert.deepEqual(officialRecovery.concurrency, {group: 'official-release-${{ inputs.tag }}', queue: 'max'});
assert.deepEqual(officialRecovery.on.workflow_dispatch.inputs.operation.options,
  ['rebuild-draft', 'resume-upload', 'replace-unpublished-assets', 'publish-verified-draft']);
assert.equal(officialRecovery.jobs.plan.environment, 'release-automation');
assert.equal(officialRecovery.jobs.mutate.environment, 'release-automation');
assert.equal(officialRecovery.jobs['load-remote'].environment, 'release-automation',
  'publish-verified-draft remote load must mint the App token inside release-automation');
const loadRemoteToken = officialRecovery.jobs['load-remote'].steps.find(step => step.id === 'app-token');
assert.equal(loadRemoteToken.with['permission-contents'], 'write');
const loadRemoteDraft = officialRecovery.jobs['load-remote'].steps.find(
  step => step.name === 'Materialize and authenticate the complete remote draft');
assert.equal(loadRemoteDraft.env.GH_TOKEN, '${{ steps.app-token.outputs.token }}',
  'publish-verified-draft must download unpublished draft assets with the App token');
assert.ok(officialRecovery.jobs.mutate.if.includes("vars.OFFICIAL_RECOVERY_READY == 'true'"));
const officialRecoverySource = read('.github/workflows/release-recovery.yml');
assert.ok(officialRecoverySource.includes('.prerelease == false'), 'official recovery must reject prerelease drafts');
assert.equal(officialRecovery.jobs.plan.steps.find(step => step.uses?.startsWith('actions/checkout@')).with.ref, '${{ github.workflow_sha }}');
assert.ok(officialRecovery.jobs['collect-rebuild'].steps.some(step => typeof step.run === 'string' &&
  step.run.includes('bash scripts/assemble-official-candidate.sh release-source/target/distrib identity.json dist-manifest.json candidate')),
  'official recovery collect must assemble the eleven-file candidate from contract-named dist outputs');
assert.ok(!officialRecoverySource.includes("jq -r '.upload_files[]' dist-manifest.json"),
  'official recovery must not copy dist host upload_files as the release candidate');
for (const jobName of ['collect-rebuild', 'load-retained', 'load-remote', 'mutate']) {
  const job = officialRecovery.jobs[jobName];
  assert.equal(job.steps.find(step => step.uses?.startsWith('actions/checkout@')).with.ref, '${{ github.workflow_sha }}',
    `official recovery ${jobName} must execute immutable dispatch control code`);
  assert.ok(job.steps.some(step => step.name?.includes('separately from protected control code')),
    `official recovery ${jobName} must materialize historical source separately`);
}
assert.ok(officialRecoverySource.includes('starter)'), 'official recovery must classify starter assets without downloading them');
assert.ok(!officialRecoverySource.includes('-f ref=refs/tags/'), 'official recovery must never create or move a tag');
assert.ok(!officialRecoverySource.includes('--method DELETE "repos/${{ github.repository }}/releases/$RELEASE_ID"'), 'official recovery must never delete a release');
assert.ok(officialRecoverySource.includes('retained-plan/official-recovery-plan.json') &&
  officialRecoverySource.includes('expectedAssets:$expected') && officialRecoverySource.includes('planSha256:$plan'),
  'official destructive recovery intent must bind its retained plan and candidate digests');
assert.ok(officialRecoverySource.includes("'.tag == $tag and .sourceSha == $sha and .releaseId == $id' candidate/codex-warp-release-metadata.json"),
  'remote official draft candidate must bind to the requested release identity');
assert.ok(officialRecoverySource.includes('cmp "candidate/$name" "remote-final/$name"'),
  'official recovery publication must bind final remote bytes to its candidate');
assert.ok(officialRecovery.jobs.rebuild.steps.some(step =>
  step.name === 'enable windows longpaths' && typeof step.run === 'string' &&
  step.run.includes('core.longpaths true')),
  'official recovery Windows rebuilds must enable git long paths before checkout');
assert.ok(officialRecoverySource.includes('scripts/verify-official-attestation.sh "$subject"'),
  'official recovery must bind candidate attestations to a trusted release workflow identity');

const nightlyRecovery = parse('.github/workflows/nightly-recovery.yml');
assert.deepEqual(Object.keys(nightlyRecovery.on), ['workflow_dispatch']);
assert.deepEqual(nightlyRecovery.concurrency, {group: 'nightly-release', queue: 'max'});
assert.deepEqual(nightlyRecovery.on.workflow_dispatch.inputs.operation.options,
  ['resume-draft', 'replace-unpublished-draft', 'recover-orphan-tag', 'repair-branch']);
assert.equal(nightlyRecovery.on.workflow_dispatch.inputs.evidence_manifest_sha256.default, '');
assert.ok(nightlyRecovery.jobs['mutate-release'].if.includes("vars.NIGHTLY_RECOVERY_READY == 'true'"));
assert.ok(nightlyRecovery.jobs['repair-branch'].if.includes("vars.NIGHTLY_RECOVERY_READY == 'true'"));
assert.equal(nightlyRecovery.jobs.plan.environment, 'release-automation',
  'nightly recovery plan must mint the App token inside release-automation');
assert.equal(nightlyRecovery.jobs['mutate-release'].environment, 'release-automation');
assert.equal(nightlyRecovery.jobs['repair-branch'].environment, 'release-automation');
const nightlyPlanToken = nightlyRecovery.jobs.plan.steps.find(step => step.id === 'app-token');
assert.equal(nightlyPlanToken.with['client-id'], '${{ vars.RELEASE_APP_CLIENT_ID }}');
assert.equal(nightlyPlanToken.with['permission-contents'], 'write');
assert.deepEqual(nightlyRecovery.jobs.build.strategy.matrix.include.map(item => item.target).sort(),
  contract.targets.map(item => item.triple).sort());
const nightlyRecoverySource = read('.github/workflows/nightly-recovery.yml');
assert.ok(nightlyRecoverySource.includes('releases/$RELEASE_INPUT'),
  'nightly recovery plan must read an unpublished draft by ID with the App token');
assert.ok(nightlyRecovery.jobs['mutate-release'].steps.some(step => step.id === 'app-token-read'),
  'nightly recovery mutate must mint an App token before observing unpublished drafts');
assert.ok(!nightlyRecoverySource.includes('-f ref=refs/tags/'), 'nightly recovery must never create or move a tag');
assert.equal(nightlyRecovery.jobs.plan.steps.find(step => step.uses?.startsWith('actions/checkout@')).with.ref, '${{ github.workflow_sha }}');
for (const jobName of ['load-origin', 'mutate-release', 'repair-branch']) {
  const job = nightlyRecovery.jobs[jobName];
  assert.equal(job.steps.find(step => step.uses?.startsWith('actions/checkout@')).with.ref, '${{ github.workflow_sha }}',
    `nightly recovery ${jobName} must execute immutable dispatch control code`);
  assert.ok(job.steps.some(step => step.name?.includes('separately from protected control code')),
    `nightly recovery ${jobName} must materialize historical source separately`);
}
assert.ok(nightlyRecoverySource.includes('/attempts/$ORIGIN_RUN_ATTEMPT/jobs?per_page=100'));
assert.ok(nightlyRecoverySource.includes('origin_manifest="$EVIDENCE_MANIFEST_SHA256"; build=false; origin=true'),
  'nightly resume-draft must reuse an authenticated retained candidate instead of rebuilding');
assert.ok(nightlyRecoverySource.includes('candidateAssets:$candidate_assets') &&
  nightlyRecoverySource.includes('planSha256:$plan') && nightlyRecoverySource.includes('remoteAssets:$remote_assets'),
  'nightly replacement intent must bind its plan, local candidate, and remote state');
assert.ok(nightlyRecoverySource.includes('cmp "candidate/$name" "remote-final/$name"'),
  'nightly recovery publication must bind final remote bytes to its candidate');
assert.deepEqual(nightlyRecovery.jobs['repair-branch'].permissions,
  {actions: 'read', attestations: 'read', contents: 'read'},
  'nightly recovery branch repair must read Actions runs to bind archive attestations');
assert.ok(nightlyRecoverySource.includes('GH_TOKEN="${{ github.token }}" bash scripts/verify-nightly-attestation.sh'),
  'nightly recovery remote archive verification must use the job token, not the mutation App token');
assert.ok(nightlyRecoverySource.includes('scripts/verify-nightly-attestation.sh'),
  'nightly recovery must bind archive attestations to a trusted nightly workflow identity');
assert.ok(nightlyRecoverySource.includes('.workflow==$f.workflow and .workflowSha==$f.workflowSha'),
  'nightly recovery collect must require cross-target workflow provenance equality');
for (const jobName of ['mutate-release', 'repair-branch']) {
  assert.ok(nightlyRecovery.jobs[jobName].steps.some(step => step.run?.includes('scripts/advance-nightly-branch.sh')),
    `nightly recovery ${jobName} must use the exact API branch race protocol`);
}
assert.ok(releaseSource.match(/\.prerelease == false/g)?.length >= 3,
  'official release must keep stable drafts non-prerelease through every mutation boundary');
assert.ok(releaseSource.includes('remote asset is not complete:'),
  'official release must reject starter and unknown remote asset states');
assert.ok(releaseSource.includes('cmp "release-assets/$name" "remote-publish/$name"') &&
  releaseSource.includes('scripts/verify-official-attestation.sh "$subject"'),
  'official publication must bind final remote bytes and attestations to the candidate');

console.log('validate-workflows: pins, permissions, triggers, recovery, matrices, and mutation gates ok');
