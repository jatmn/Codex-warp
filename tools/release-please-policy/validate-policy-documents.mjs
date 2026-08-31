import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import Ajv2020 from 'ajv/dist/2020.js';
import addFormats from 'ajv-formats';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../..');
const read = relative => JSON.parse(fs.readFileSync(path.join(root, relative)));
const digest = relative => crypto.createHash('sha256')
  .update(fs.readFileSync(path.join(root, relative))).digest('hex');

const ajv2020 = new Ajv2020({allErrors: true, strict: false});
addFormats(ajv2020);
const validatePolicy = ajv2020.compile(read('tools/release-automation-policy.schema.json'));
const policy = read('tools/release-automation-policy.json');
assert.equal(validatePolicy(policy), true, ajv2020.errorsText(validatePolicy.errors));
assert.ok(policy.requiredFiles.every(file => policy.allowedFiles.includes(file)));
assert.equal(policy.enabled, false, 'live App identity must be reviewed before enabling policy');
assert.equal(policy.appBot.id, null);
assert.equal(policy.appBot.login, null);

const tooling = read('tools/release-tooling.json');
const policyPackage = read('tools/release-please-policy/package.json');
const policyLock = read('tools/release-please-policy/package-lock.json');
assert.equal(digest('tools/release-please-policy/config.schema.json'), tooling.releasePlease.configSchemaSha256);
assert.equal(digest('tools/dist-manifest.schema.json'), tooling.dist.manifestSchemaSha256);
assert.equal(tooling.releasePlease.version, '17.6.0');
assert.equal(tooling.dist.version, '0.32.0');
assert.equal(policyPackage.dependencies.ajv, '8.20.0');
assert.equal(policyPackage.dependencies['release-please'], undefined);
assert.equal(policyPackage.devDependencies['release-please'], tooling.releasePlease.version);
assert.equal(policyLock.packages['node_modules/yargs'].version, '17.7.3');
assert.equal(
  policyLock.packages['node_modules/yargs'].integrity,
  'sha512-GZtjxm/J/4TSxuL3FNYjCmLktBTnIw/rVmKSIyKeYAZpmJB2ig9VauCC5xsa82GNKVKDAqpOn3KVzNt0zmrU0g=='
);

const distAjv = new Ajv2020({allErrors: true, strict: false});
addFormats(distAjv);
distAjv.addFormat('uint64', {
  type: 'number',
  validate: value => Number.isSafeInteger(value) && value >= 0,
});
const validateDist = distAjv.compile(read('tools/dist-manifest.schema.json'));
const officialManifest = read('tools/release-please-policy/fixtures/dist-manifest.official.json');
assert.equal(validateDist(officialManifest), true, distAjv.errorsText(validateDist.errors));

const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'codex-warp-policy-'));
try {
  const officialOutput = path.join(temp, 'official.json');
  const generated = spawnSync('bash', [
    path.join(root, 'scripts/generate-release-metadata.sh'),
    'official',
    path.join(root, 'tools/release-please-policy/fixtures/metadata-identity.official.json'),
    path.join(root, 'tools/release-please-policy/fixtures/dist-manifest.official.json'),
    officialOutput,
  ], {encoding: 'utf8'});
  assert.equal(generated.status, 0, generated.stderr);

  const validateMetadata = ajv2020.compile(read('tools/release-metadata.schema.json'));
  const official = JSON.parse(fs.readFileSync(officialOutput));
  assert.equal(validateMetadata(official), true, ajv2020.errorsText(validateMetadata.errors));
  assert.deepEqual(
    [...new Set(official.dist.artifacts.map(item => item.target))].sort(),
    official.dist.artifacts.map(item => item.target).sort()
  );
  for (const artifact of official.dist.artifacts) {
    assert.equal(artifact.checksumFile, `${artifact.archive}.sha256`);
  }

  const proofIdentity = structuredClone(read('tools/release-please-policy/fixtures/metadata-identity.official.json'));
  proofIdentity.publishable = false;
  proofIdentity.tag = null;
  proofIdentity.peeledTagSha = null;
  proofIdentity.releaseId = null;
  proofIdentity.pullRequest = {
    number: 7,
    baseSha: '6666666666666666666666666666666666666666',
    headSha: '7777777777777777777777777777777777777777',
    buildSourceSha: proofIdentity.sourceSha,
    mergeSha: '8888888888888888888888888888888888888888',
  };
  const proofIdentityPath = path.join(temp, 'proof-identity.json');
  const proofManifestPath = path.join(temp, 'proof-manifest.json');
  const proofOutput = path.join(temp, 'proof.json');
  fs.writeFileSync(proofIdentityPath, JSON.stringify(proofIdentity));
  fs.writeFileSync(proofManifestPath, JSON.stringify({...officialManifest, announcement_tag_is_implicit: true}));
  const generatedProof = spawnSync('bash', [
    path.join(root, 'scripts/generate-release-metadata.sh'),
    'pr-upload-proof', proofIdentityPath, proofManifestPath, proofOutput,
  ], {encoding: 'utf8'});
  assert.equal(generatedProof.status, 0, generatedProof.stderr);
  const proof = JSON.parse(fs.readFileSync(proofOutput));
  assert.equal(validateMetadata(proof), true, ajv2020.errorsText(validateMetadata.errors));

  const proofClaimingPublication = {...proof, publishable: true};
  assert.equal(validateMetadata(proofClaimingPublication), false);
  const officialWithImplicitTag = structuredClone(official);
  officialWithImplicitTag.dist.announcementTagIsImplicit = true;
  assert.equal(validateMetadata(officialWithImplicitTag), false);
} finally {
  fs.rmSync(temp, {recursive: true, force: true});
}

const validateRecovery = ajv2020.compile(read('tools/recovery-recipes/schema.json'));
const validRecovery = read('tools/release-please-policy/fixtures/recovery.valid.json');
assert.equal(validateRecovery(validRecovery), true, ajv2020.errorsText(validateRecovery.errors));
for (const invalid of ['recovery.invalid-identity.json', 'recovery.invalid-schema.json']) {
  assert.equal(validateRecovery(read(`tools/release-please-policy/fixtures/${invalid}`)), false, invalid);
}
for (const inputs of [validRecovery.original, validRecovery.replacement]) {
  const schema = inputs.tool.manifestSchema;
  assert.equal(path.basename(schema.vendoredPath), `${schema.sha256}.json`);
  assert.equal(digest(schema.vendoredPath), schema.sha256);
}

const contract = read('tools/release-contract.json');
assert.equal(contract.targets.length, 4);
assert.equal(new Set(contract.targets.map(item => item.triple)).size, 4);
assert.equal(new Set(contract.targets.map(item => item.archive)).size, 4);
assert.ok(contract.requiredArchiveEntries.includes('NOTICE'));
assert.ok(contract.requiredArchiveEntries.includes('CHANGELOG.md'));

const validateNightly = ajv2020.compile(read('tools/nightly-manifest.schema.json'));
const nightlySha = '1234567890abcdef1234567890abcdef12345678';
const nightlyDigest = 'a'.repeat(64);
const nightlyTag = `nightly-20260830-${nightlySha.slice(0, 12)}`;
const nightly = {
  $schema: './nightly-manifest.schema.json', schemaVersion: 1,
  fileName: 'codex-warp-nightly-manifest.json', repository: 'jatmn/Codex-warp',
  tag: nightlyTag, date: '20260830', sourceSha: nightlySha, baseVersion: '0.0.1',
  version: `0.0.1-nightly.20260830.${nightlySha.slice(0, 12)}`,
  workflow: 'https://github.com/jatmn/Codex-warp/actions/runs/1', workflowSha: nightlySha,
  cargoLockSha256: nightlyDigest, rustToolchainSha256: nightlyDigest,
  packagingContractSha256: nightlyDigest, packagingScriptSha256: nightlyDigest,
  artifacts: contract.targets.map(({triple}) => ({
    target: triple,
    archive: `codex-warp-${nightlyTag}-${triple}.${triple.endsWith('windows-msvc') ? 'zip' : 'tar.xz'}`,
    archiveSha256: nightlyDigest,
    checksumFile: `codex-warp-${nightlyTag}-${triple}.${triple.endsWith('windows-msvc') ? 'zip' : 'tar.xz'}.sha256`,
    runnerLabel: 'fixture', runnerImage: 'fixture', rustcVv: 'rustc fixture',
    cargoVv: 'cargo fixture', nativeTools: 'fixture', packagingTools: {fixture: nightlyDigest},
  })),
};
assert.equal(validateNightly(nightly), true, ajv2020.errorsText(validateNightly.errors));
assert.equal(validateNightly({...nightly, sourceSha: 'f'.repeat(39)}), false);

console.log('validate-policy-documents: schemas, metadata modes, nightly, recovery, and digests ok');
