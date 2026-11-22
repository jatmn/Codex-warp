import { appendFileSync } from 'node:fs';
import { setTimeout as wait } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
const OWNER_ID = 12479882;

const REVIEW_TRIGGERS = new Set([
  'pull_request_opened',
  'pull_request_ready_for_review',
  'pull_request_synchronize',
]);

export async function isAuthorizedReview(payload, { apiUrl, repository, token }) {
  const event = payload?.event;
  if (payload?.['~pullfrog'] !== true || event?.is_pr !== true ||
      !REVIEW_TRIGGERS.has(event.trigger) ||
      !Number.isSafeInteger(event.issue_number) || event.issue_number <= 0) {
    return false;
  }

  // Read the PR author from GitHub because a different account may have pushed the commit.
  const url = `${apiUrl}/repos/${repository}/pulls/${event.issue_number}`;
  const headers = { Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json' };
  let lastError;
  for (const delay of [0, 250, 750]) {
    if (delay > 0) await wait(delay);
    try {
      const response = await fetch(url, { headers, signal: AbortSignal.timeout(10_000) });
      if (response.ok) {
        const pr = await response.json();
        return pr?.user?.id === OWNER_ID && pr.user.login === 'jatmn';
      }

      lastError = new Error(`GitHub HTTP ${response.status}`);
      const retryable = response.status === 408 || response.status >= 500;
      if (!retryable || response.headers.has('Retry-After')) break;
      await response.body?.cancel();
    } catch (error) {
      lastError = error;
      if (!(error instanceof TypeError || error?.name === 'TimeoutError' || error?.name === 'AbortError')) break;
    }
  }
  throw new Error(`Could not verify PR #${event.issue_number} author.`, { cause: lastError });
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const authorized = await isAuthorizedReview(JSON.parse(process.env.PULLFROG_PROMPT), {
    apiUrl: process.env.GITHUB_API_URL,
    repository: process.env.GITHUB_REPOSITORY,
    token: process.env.GH_TOKEN,
  });
  if (authorized) {
    appendFileSync(process.env.GITHUB_OUTPUT, 'authorized=true\n');
  } else {
    console.log('Skipping automatic review. The dispatch must target an owner-authored PR.');
  }
}
