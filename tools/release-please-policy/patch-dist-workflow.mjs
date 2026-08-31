import fs from 'node:fs';
import path from 'node:path';
import YAML from 'yaml';

if (process.argv.length !== 3) {
  console.error('usage: patch-dist-workflow.mjs <release.yml>');
  process.exit(2);
}
const workflowPath = path.resolve(process.argv[2]);
let source = fs.readFileSync(workflowPath, 'utf8');
const parsed = YAML.parseDocument(source);
if (parsed.errors.length > 0) throw parsed.errors[0];
const doc = parsed.toJS();
if (source.includes('# Prepare and validate the complete candidate without mutation credentials.')) {
  if (source.includes('cargo-dist-installer.sh') || source.includes('cargo-dist-installer.ps1')) {
    throw new Error('patched workflow contains an unverified dist installer');
  }
  if (/permissions:\n\s+["']?contents["']?: ["']?write/.test(source)) {
    throw new Error('patched workflow contains a GITHUB_TOKEN contents:write grant');
  }
  console.log('patch-dist-workflow: overlay already applied');
  process.exit(0);
}
if (doc.name !== 'Release' || !doc.jobs?.plan || !doc.jobs?.['build-local-artifacts'] || !doc.jobs?.host) {
  throw new Error('pinned dist template anchors drifted');
}

function replaceOnce(before, after, label) {
  const first = source.indexOf(before);
  if (first < 0 || source.indexOf(before, first + before.length) >= 0) {
    throw new Error(`${label} anchor must occur exactly once`);
  }
  source = source.replace(before, after);
}

replaceOnce('permissions:\n  "contents": "write"', 'permissions:\n  contents: read', 'root permissions');
replaceOnce("      - '**[0-9]+.[0-9]+.[0-9]+*'", "      - 'v[0-9]+.[0-9]+.[0-9]+'", 'tag glob');
replaceOnce(
  "      - 'v[0-9]+.[0-9]+.[0-9]+'\n\njobs:",
  "      - 'v[0-9]+.[0-9]+.[0-9]+'\n\nconcurrency:\n  group: official-release-\${{ github.event.pull_request.number || github.ref_name }}\n  queue: max\n\njobs:",
  'workflow concurrency'
);
replaceOnce(
  `      - name: Install dist
        # we specify bash to get pipefail; it guards against the \`curl\` command
        # failing. otherwise \`sh\` won't catch that \`curl\` returned non-0
        shell: bash
        run: "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/axodotdev/cargo-dist/releases/download/v0.32.0/cargo-dist-installer.sh | sh"`,
  `      - name: Validate Trigger
        shell: bash
        run: |
          if [ "\${{ github.event_name }}" = push ]; then
            [[ "\${{ github.ref_name }}" =~ ^v[0-9]+\\.[0-9]+\\.[0-9]+$ ]]
          fi
      - name: Install verified dist
        shell: bash
        run: bash scripts/install-pinned-dist.sh`,
  'plan installer'
);
replaceOnce(
  `      - name: Install Rust non-interactively if not already installed
        if: \${{ matrix.container }}
        run: |
          if ! command -v cargo > /dev/null 2>&1; then
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
            echo "$HOME/.cargo/bin" >> $GITHUB_PATH
          fi`,
  `      - name: Confirm pinned Rust
        shell: bash
        run: |
          rustup show active-toolchain
          rustc -Vv
          cargo -V`,
  'build rust installer'
);
if (source.includes('uses: swatinem/rust-cache@v2')) {
  replaceOnce(
    'uses: swatinem/rust-cache@v2',
    'uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2',
    'build cache pin'
  );
} else if (source.includes('uses: swatinem/rust-cache@')) {
  throw new Error('build cache pin drifted');
}
replaceOnce(
  `      - name: Install dist
        run: \${{ matrix.install_dist.run }}`,
  `      - name: Install verified dist
        shell: bash
        run: bash scripts/install-pinned-dist.sh`,
  'build dist installer'
);
if (source.includes('uses: actions/attest@v4')) {
  replaceOnce('uses: actions/attest@v4', 'uses: actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6 # v4.2.2', 'attestation pin');
} else if (!source.includes('uses: actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6')) {
  throw new Error('attestation pin anchor drifted');
}
replaceOnce(
  `      - name: "Upload artifacts"
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
        with:
          name: artifacts-build-local-\${{ join(matrix.targets, '_') }}
          path: |
            \${{ steps.cargo-dist.outputs.paths }}
            \${{ env.BUILD_MANIFEST_NAME }}`,
  `      - name: Record runner evidence
        shell: bash
        run: |
          evidence="target/distrib/\${{ join(matrix.targets, '-') }}-runner.json"
          native_tools="$({ cmake --version 2>/dev/null | head -1; xcodebuild -version 2>/dev/null | paste -sd ' ' -; nasm -v 2>/dev/null; cc --version 2>/dev/null | head -1; } || true)"
          jq -n \\
            --arg target "\${{ join(matrix.targets, '') }}" \\
            --arg label "\${{ matrix.runner }}" \\
            --arg image "\${ImageOS:-unknown}/\${ImageVersion:-unknown}" \\
            --arg native "$native_tools" \\
            --arg rustc "$(rustc -Vv)" \\
            --arg cargo "$(cargo -Vv)" \\
            '{target:$target,runnerLabel:$label,runnerImage:$image,nativeTools:{observed:$native},rustcVv:$rustc,cargoVv:$cargo}' >"$evidence"
      - name: "Upload artifacts"
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
        with:
          name: artifacts-build-local-\${{ join(matrix.targets, '_') }}
          path: |
            \${{ steps.cargo-dist.outputs.paths }}
            \${{ env.BUILD_MANIFEST_NAME }}
            target/distrib/\${{ join(matrix.targets, '-') }}-runner.json`,
  'runner evidence'
);

const hostStart = source.indexOf('  # Determines if we should publish/announce\n  host:');
if (hostStart < 0) throw new Error('host tail anchor drifted');
source = source.slice(0, hostStart) + `  # Prepare and validate the complete candidate without mutation credentials.
  prepare-official-release:
    needs: [plan, build-local-artifacts, build-global-artifacts]
    if: \${{ always() && github.event_name == 'push' && github.ref_type == 'tag' && needs.plan.result == 'success' && needs.build-local-artifacts.result == 'success' && needs.build-global-artifacts.result == 'success' }}
    runs-on: ubuntu-24.04
    permissions:
      contents: read
    outputs:
      release_id: \${{ steps.identity.outputs.release_id }}
    env:
      GH_TOKEN: \${{ github.token }}
      TAG: \${{ github.ref_name }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          fetch-depth: 0
          persist-credentials: false
      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          node-version: 24.20.0
          cache: npm
          cache-dependency-path: tools/release-please-policy/package-lock.json
      - run: npm ci --omit=dev --ignore-scripts --no-audit --no-fund
        working-directory: tools/release-please-policy
      - uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1
        with:
          name: cargo-dist-cache
          path: ~/.cargo/bin/
      - run: chmod +x ~/.cargo/bin/dist
      - uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1
        with:
          pattern: artifacts-*
          path: target/distrib/
          merge-multiple: true
      - name: Prepare final unmodified dist manifest
        shell: bash
        run: |
          [[ "$TAG" =~ ^v[0-9]+\\.[0-9]+\\.[0-9]+$ ]]
          dist host --tag="$TAG" --steps=upload --steps=release --output-format=json >dist-manifest.json
          node tools/release-please-policy/validate-json.mjs tools/dist-manifest.schema.json dist-manifest.json
      - id: identity
        name: Bind tag, draft, source, tools, and runners
        shell: bash
        env:
          WORKFLOW_SHA: \${{ github.workflow_sha }}
        run: |
          source_sha="$(git rev-parse HEAD)"
          peeled="$(git rev-parse "refs/tags/$TAG^{}")"
          [ "$source_sha" = "$peeled" ]
          version="$(sed -n 's/^version = "\\([^"]*\\)"/\\1/p' Cargo.toml | head -1)"
          [ "$TAG" = "v$version" ]
          release=''
          for attempt in {1..12}; do
            if release="$(gh api "repos/\${{ github.repository }}/releases/tags/$TAG" 2>/dev/null)"; then
              break
            fi
            [ "$attempt" -lt 12 ] && sleep 5
          done
          [ -n "$release" ] || { echo "Release Please did not create the expected draft for $TAG" >&2; exit 1; }
          jq -e --arg tag "$TAG" '.tag_name == $tag and .draft == true and .published_at == null' <<<"$release" >/dev/null
          release_id="$(jq -r '.id' <<<"$release")"
          runners="$(jq -sc 'sort_by(.target)' target/distrib/*-runner.json)"
          [ "$(jq 'length' <<<"$runners")" -eq 4 ]
          jq -n \\
            --argjson publishable true \\
            --arg repository "\${{ github.repository }}" \\
            --arg source "$source_sha" --arg version "$version" \\
            --arg lock "$(sha256sum Cargo.lock | awk '{print $1}')" \\
            --arg toolchain "$(sha256sum rust-toolchain.toml | awk '{print $1}')" \\
            --arg channel "$(sed -n 's/^channel = "\\([^"]*\\)"/\\1/p' rust-toolchain.toml)" \\
            --arg rustc "$(rustc -Vv)" --arg cargo "$(cargo -Vv)" \\
            --arg contract "$(sha256sum tools/release-contract.json | awk '{print $1}')" \\
            --arg workflow_sha "$WORKFLOW_SHA" \\
            --arg tag "$TAG" --argjson release_id "$release_id" \\
            --arg schema "$(sha256sum tools/dist-manifest.schema.json | awk '{print $1}')" \\
            --argjson runners "$runners" \\
            '{publishable:$publishable,repository:$repository,sourceSha:$source,cargoVersion:$version,cargoLockSha256:$lock,rustToolchain:{fileSha256:$toolchain,channel:$channel,rustcVv:$rustc,cargoVv:$cargo},releaseContractSha256:$contract,workflow:{name:"Release",runId:\${{ github.run_id }},runAttempt:\${{ github.run_attempt }},workflowSha:$workflow_sha},dist:{version:"0.32.0",manifestSchemaSha256:$schema},runners:$runners,tag:$tag,peeledTagSha:$source,releaseId:$release_id,pullRequest:null}' >identity.json
          echo "release_id=$release_id" >>"$GITHUB_OUTPUT"
      - name: Assemble and validate exact asset set
        shell: bash
        run: |
          mkdir release-assets
          while IFS= read -r file; do
            [ -f "$file" ]
            cp "$file" release-assets/
          done < <(jq -r '.upload_files[]' dist-manifest.json)
          cp dist-manifest.json release-assets/dist-manifest.json
          bash scripts/generate-release-metadata.sh official identity.json dist-manifest.json release-assets/codex-warp-release-metadata.json
          SOURCE_DIR="$PWD" bash scripts/check-release-contract.sh official-publication release-assets release-assets/codex-warp-release-metadata.json release-assets/dist-manifest.json
      - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: official-release-candidate
          path: release-assets/
          if-no-files-found: error

  attest-official-metadata:
    needs: prepare-official-release
    runs-on: ubuntu-24.04
    permissions:
      attestations: write
      contents: read
      id-token: write
    steps:
      - uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1
        with:
          name: official-release-candidate
          path: release-assets
      - uses: actions/attest-build-provenance@4d101475d8b20a2381f78447822ac1eab6504dd8 # v4.2.2
        with:
          subject-path: release-assets/codex-warp-release-metadata.json

  publish-official-release:
    needs: [prepare-official-release, attest-official-metadata]
    if: \${{ github.event_name == 'push' && github.ref_type == 'tag' && vars.OFFICIAL_RELEASES_ENABLED == 'true' }}
    runs-on: ubuntu-24.04
    environment: release-automation
    permissions:
      attestations: read
      contents: read
    env:
      GH_TOKEN: \${{ github.token }}
      TAG: \${{ github.ref_name }}
      RELEASE_ID: \${{ needs.prepare-official-release.outputs.release_id }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          fetch-depth: 0
          persist-credentials: false
      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          node-version: 24.20.0
          cache: npm
          cache-dependency-path: tools/release-please-policy/package-lock.json
      - run: npm ci --omit=dev --ignore-scripts --no-audit --no-fund
        working-directory: tools/release-please-policy
      - uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1
        with:
          name: official-release-candidate
          path: release-assets
      - name: Revalidate candidate and live draft before private key use
        shell: bash
        run: |
          SOURCE_DIR="$PWD" bash scripts/check-release-contract.sh official-publication release-assets release-assets/codex-warp-release-metadata.json release-assets/dist-manifest.json
          [ "$(jq -r '.enabled' tools/release-automation-policy.json)" = true ]
          [ "$(git rev-parse "refs/tags/$TAG^{}")" = "$(jq -r '.sourceSha' release-assets/codex-warp-release-metadata.json)" ]
          release="$(gh api "repos/\${{ github.repository }}/releases/$RELEASE_ID")"
          jq -e --arg tag "$TAG" --argjson id "$RELEASE_ID" '.id == $id and .tag_name == $tag and .draft == true and .published_at == null' <<<"$release" >/dev/null
          mkdir remote-before
          while IFS=$'\\t' read -r id name; do
            [ -f "release-assets/$name" ] || { echo "unexpected remote asset: $name" >&2; exit 1; }
            gh api -H 'Accept: application/octet-stream' "repos/\${{ github.repository }}/releases/assets/$id" >"remote-before/$name"
            [ "$(sha256sum "remote-before/$name" | awk '{print $1}')" = "$(sha256sum "release-assets/$name" | awk '{print $1}')" ] || { echo "mismatched remote asset: $name" >&2; exit 1; }
          done < <(gh api --paginate "repos/\${{ github.repository }}/releases/$RELEASE_ID/assets" --jq '.[] | [.id,.name] | @tsv')
          for subject in release-assets/*.tar.xz release-assets/*.zip release-assets/codex-warp-release-metadata.json; do
            gh attestation verify "$subject" --repo "\${{ github.repository }}"
          done
      - id: app-token
        name: Mint bounded release App token
        uses: actions/create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1 # v3.2.0
        with:
          client-id: \${{ vars.RELEASE_APP_CLIENT_ID }}
          private-key: \${{ secrets.RELEASE_APP_PRIVATE_KEY }}
          owner: \${{ github.repository_owner }}
          repositories: Codex-warp
          permission-contents: write
          permission-workflows: write
      - name: Upload only missing verified assets
        shell: bash
        env:
          GH_TOKEN: \${{ steps.app-token.outputs.token }}
        run: |
          for file in release-assets/*; do
            name="$(basename "$file")"
            count="$(gh api --paginate "repos/\${{ github.repository }}/releases/$RELEASE_ID/assets" | jq -s --arg name "$name" '[.[][] | select(.name == $name)] | length')"
            [ "$count" -le 1 ]
            if [ "$count" -eq 0 ]; then
              gh release upload "$TAG" "$file"
            fi
          done
      - name: Verify complete remote checksums
        shell: bash
        env:
          GH_TOKEN: \${{ github.token }}
        run: |
          mkdir remote-final
          gh api --paginate "repos/\${{ github.repository }}/releases/$RELEASE_ID/assets" --jq '.[].name' | sort >remote-names.txt
          find release-assets -maxdepth 1 -type f -printf '%f\\n' | sort >expected-names.txt
          cmp expected-names.txt remote-names.txt
          while IFS=$'\\t' read -r id name; do
            gh api -H 'Accept: application/octet-stream' "repos/\${{ github.repository }}/releases/assets/$id" >"remote-final/$name"
            [ "$(sha256sum "remote-final/$name" | awk '{print $1}')" = "$(sha256sum "release-assets/$name" | awk '{print $1}')" ]
          done < <(gh api --paginate "repos/\${{ github.repository }}/releases/$RELEASE_ID/assets" --jq '.[] | [.id,.name] | @tsv')
      - name: Publish exact verified draft
        shell: bash
        env:
          GH_TOKEN: \${{ steps.app-token.outputs.token }}
        run: |
          release="$(gh api "repos/\${{ github.repository }}/releases/$RELEASE_ID")"
          jq -e --arg tag "$TAG" --argjson id "$RELEASE_ID" '.id == $id and .tag_name == $tag and .draft == true and .published_at == null' <<<"$release" >/dev/null
          gh api --method PATCH "repos/\${{ github.repository }}/releases/$RELEASE_ID" -f draft=false >/dev/null

  announce:
    needs: publish-official-release
    if: \${{ always() && needs.publish-official-release.result == 'success' }}
    runs-on: ubuntu-24.04
    permissions:
      contents: read
    steps:
      - run: echo "Published verified official release \${{ github.ref_name }}" >>"$GITHUB_STEP_SUMMARY"
`;

const finalDocument = YAML.parseDocument(source);
if (finalDocument.errors.length > 0) throw finalDocument.errors[0];
if (source.includes('cargo-dist-installer.sh') || source.includes('cargo-dist-installer.ps1')) {
  throw new Error('unverified dist installer remains after overlay');
}
if (/permissions:\n\s+["']?contents["']?: ["']?write/.test(source)) {
  throw new Error('GITHUB_TOKEN contents:write grant remains after overlay');
}
fs.writeFileSync(workflowPath, source);
