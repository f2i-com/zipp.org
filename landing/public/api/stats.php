<?php
declare(strict_types=1);

/**
 * Live repository facts for the landing page.
 *
 * Fetches the public GitHub API and the README on `main`, distils the figures
 * the page shows (version, commit count, stars, latest release, the canonical
 * capture numbers and the test262 tally), caches the result for CACHE_TTL
 * seconds, and answers JSON. On any upstream failure the last good cache is
 * served with `"stale": true`; with no cache at all the page keeps its
 * built-in figures, because the frontend treats every field as optional.
 *
 * Requirements: PHP 8.1+, the curl extension (or allow_url_fopen), and a
 * writable cache location (`api/cache/` beside this file, else the system
 * temp directory). Set ZIPP_GITHUB_TOKEN in the server environment to lift
 * GitHub's anonymous rate limit (60 requests/hour per address; this script
 * makes five per cache miss).
 */

const REPO = 'f2i-com/zipp.org';
const BRANCH = 'main';
const CACHE_TTL = 900;          // fresh for 15 minutes
const STALE_MAX_AGE = 7 * 86400; // serve a failed refresh from cache up to a week

header('Content-Type: application/json; charset=utf-8');
header('Cache-Control: public, max-age=300');
header('X-Content-Type-Options: nosniff');

$cacheDir = is_writable(__DIR__ . '/cache') ? __DIR__ . '/cache' : sys_get_temp_dir();
$cacheFile = $cacheDir . '/zipp-landing-stats.json';

$cached = null;
if (is_file($cacheFile)) {
    $raw = @file_get_contents($cacheFile);
    $decoded = $raw === false ? null : json_decode($raw, true);
    if (is_array($decoded) && isset($decoded['generated_at'])) {
        $cached = $decoded;
        $age = time() - strtotime((string) $decoded['generated_at']);
        if ($age >= 0 && $age < CACHE_TTL) {
            $decoded['cached'] = true;
            echo json_encode($decoded, JSON_UNESCAPED_SLASHES | JSON_PRETTY_PRINT);
            exit;
        }
    }
}

/**
 * A CA bundle for TLS verification when php.ini names none (WAMP/XAMPP on
 * Windows ship without `curl.cainfo`; Debian, RHEL and Alpine keep theirs at
 * the paths below; Git for Windows carries one too). Verification is never
 * disabled -- with no bundle found the fetch simply fails and the page keeps
 * its built-in figures.
 */
function caBundle(): ?string
{
    foreach ([ini_get('curl.cainfo'), ini_get('openssl.cafile')] as $configured) {
        if (is_string($configured) && $configured !== '' && is_file($configured)) {
            return $configured;
        }
    }
    foreach ([
        __DIR__ . '/cacert.pem',
        '/etc/ssl/certs/ca-certificates.crt',
        '/etc/pki/tls/certs/ca-bundle.crt',
        '/etc/ssl/cert.pem',
        '/usr/local/etc/openssl/cert.pem',
        'C:/Program Files/Git/mingw64/etc/ssl/certs/ca-bundle.crt',
        'C:/Program Files (x86)/Git/mingw64/etc/ssl/certs/ca-bundle.crt',
    ] as $candidate) {
        if (is_file($candidate)) {
            return $candidate;
        }
    }
    return null;
}

/** GET a URL; returns [status, body, headers] or null on transport failure. */
function fetch(string $url): ?array
{
    static $ca = false;
    if ($ca === false) {
        $ca = caBundle();
    }
    $headers = [
        'User-Agent: zipp-landing-stats/1.0 (+https://zipp.org)',
        'Accept: application/vnd.github+json, text/plain;q=0.9, */*;q=0.5',
        'X-GitHub-Api-Version: 2022-11-28',
    ];
    $token = getenv('ZIPP_GITHUB_TOKEN');
    if (is_string($token) && $token !== '') {
        $headers[] = 'Authorization: Bearer ' . $token;
    }
    if (function_exists('curl_init')) {
        $ch = curl_init($url);
        $responseHeaders = [];
        curl_setopt_array($ch, [
            CURLOPT_RETURNTRANSFER => true,
            CURLOPT_FOLLOWLOCATION => true,
            CURLOPT_MAXREDIRS => 3,
            CURLOPT_TIMEOUT => 8,
            CURLOPT_CONNECTTIMEOUT => 4,
            CURLOPT_HTTPHEADER => $headers,
            CURLOPT_CAINFO => $ca ?? null,
            CURLOPT_HEADERFUNCTION => static function ($ch, string $line) use (&$responseHeaders): int {
                $parts = explode(':', $line, 2);
                if (count($parts) === 2) {
                    $responseHeaders[strtolower(trim($parts[0]))] = trim($parts[1]);
                }
                return strlen($line);
            },
        ]);
        $body = curl_exec($ch);
        $status = (int) curl_getinfo($ch, CURLINFO_RESPONSE_CODE);
        curl_close($ch);
        if ($body === false) {
            return null;
        }
        return [$status, (string) $body, $responseHeaders];
    }
    $ssl = ['verify_peer' => true, 'verify_peer_name' => true];
    if ($ca !== null) {
        $ssl['cafile'] = $ca;
    }
    $context = stream_context_create(['http' => [
        'method' => 'GET',
        'header' => implode("\r\n", $headers),
        'timeout' => 8,
        'ignore_errors' => true,
    ], 'ssl' => $ssl]);
    $body = @file_get_contents($url, false, $context);
    if ($body === false) {
        return null;
    }
    $status = 0;
    $responseHeaders = [];
    foreach ($http_response_header ?? [] as $line) {
        if (preg_match('#^HTTP/\S+\s+(\d{3})#', $line, $m)) {
            $status = (int) $m[1];
        } elseif (str_contains($line, ':')) {
            [$k, $v] = explode(':', $line, 2);
            $responseHeaders[strtolower(trim($k))] = trim($v);
        }
    }
    return [$status, $body, $responseHeaders];
}

function fetchJson(string $url): ?array
{
    $r = fetch($url);
    if ($r === null || $r[0] !== 200) {
        return null;
    }
    $decoded = json_decode($r[1], true);
    return is_array($decoded) ? $decoded : null;
}

$api = 'https://api.github.com/repos/' . REPO;
$rawBase = 'https://raw.githubusercontent.com/' . REPO . '/' . BRANCH . '/';

$out = [
    'generated_at' => gmdate('c'),
    'cached' => false,
    'stale' => false,
    'source' => ['repo' => REPO, 'branch' => BRANCH],
];
$failures = [];

// ── repository ──────────────────────────────────────────────────────────────
$repo = fetchJson($api);
if ($repo !== null) {
    $out['repo'] = [
        'stars' => (int) ($repo['stargazers_count'] ?? 0),
        'forks' => (int) ($repo['forks_count'] ?? 0),
        'open_issues' => (int) ($repo['open_issues_count'] ?? 0),
        'pushed_at' => $repo['pushed_at'] ?? null,
        'default_branch' => $repo['default_branch'] ?? BRANCH,
        'url' => $repo['html_url'] ?? ('https://github.com/' . REPO),
    ];
} else {
    $failures[] = 'repo';
}

// ── latest release (tag, name, date) ────────────────────────────────────────
$release = fetchJson($api . '/releases/latest');
if ($release !== null && isset($release['tag_name'])) {
    $out['release'] = [
        'tag' => $release['tag_name'],
        'name' => $release['name'] ?? $release['tag_name'],
        'published_at' => $release['published_at'] ?? null,
        'url' => $release['html_url'] ?? null,
    ];
} else {
    $failures[] = 'release';
}

// ── commit count and the latest commit on the branch ────────────────────────
$commits = fetch($api . '/commits?sha=' . BRANCH . '&per_page=1');
if ($commits !== null && $commits[0] === 200) {
    $count = null;
    if (isset($commits[2]['link']) && preg_match('/[?&]page=(\d+)>;\s*rel="last"/', $commits[2]['link'], $m)) {
        $count = (int) $m[1];
    }
    $first = json_decode($commits[1], true);
    $latest = is_array($first) && isset($first[0]) ? $first[0] : null;
    $out['commits'] = [
        'count' => $count,
        'latest' => $latest === null ? null : [
            'sha' => substr((string) ($latest['sha'] ?? ''), 0, 8),
            'message' => strtok((string) ($latest['commit']['message'] ?? ''), "\n"),
            'date' => $latest['commit']['committer']['date'] ?? null,
            'url' => $latest['html_url'] ?? null,
        ],
    ];
} else {
    $failures[] = 'commits';
}

// ── workspace version from Cargo.toml ───────────────────────────────────────
$cargo = fetch($rawBase . 'Cargo.toml');
if ($cargo !== null && $cargo[0] === 200 && preg_match('/^version\s*=\s*"([^"]+)"/m', $cargo[1], $m)) {
    $out['version'] = $m[1];
} else {
    $failures[] = 'version';
}

// ── the README's own headline figures ───────────────────────────────────────
$readme = fetch($rawBase . 'README.md');
if ($readme !== null && $readme[0] === 200) {
    $text = $readme[1];
    $facts = [];
    // | **Competes today** | Canonical equal-row all-30 geomean **0.729× Node**; normal all-13 **0.620×** and hostile all-17 **0.824×**. ...
    if (preg_match('/all-30 geomean \*\*([0-9.]+)× Node\*\*; normal all-13 \*\*([0-9.]+)×\*\* and hostile all-17 \*\*([0-9.]+)×\*\*/u', $text, $m)) {
        $facts['all30'] = (float) $m[1];
        $facts['all13'] = (float) $m[2];
        $facts['hostile17'] = (float) $m[3];
    }
    // **99.997% of test262**: 95,939 / 95,942 required executions.
    if (preg_match('/\*\*([0-9.]+)% of test262\*\*: ([0-9,]+) \/ ([0-9,]+)/', $text, $m)) {
        $facts['test262_pct'] = (float) $m[1];
        $facts['test262_pass'] = (int) str_replace(',', '', $m[2]);
        $facts['test262_total'] = (int) str_replace(',', '', $m[3]);
    }
    // Zipp has\n21 of 30 Node point wins.
    if (preg_match('/Zipp has\s+(\d+) of 30 Node point wins/', $text, $m)) {
        $facts['node_wins'] = (int) $m[1];
    }
    // ... clean PGO capture at engine commit\n`c28781cf`:
    if (preg_match('/capture at engine commit\s+`([0-9a-f]{7,40})`/', $text, $m)) {
        $facts['capture_commit'] = $m[1];
    }
    // **7.9 ms** median process launch
    if (preg_match('/\*\*([0-9.]+) ms\*\* median process launch/', $text, $m)) {
        $facts['startup_ms'] = (float) $m[1];
    }
    // Node v24.12.0 · Bun 1.3.14 · Deno 2.6.10 · Zipp 0.0.11 canonical PGO
    if (preg_match('/Node (v[0-9.]+) · Bun ([0-9.]+) · Deno ([0-9.]+) · Zipp ([0-9.]+) canonical PGO/u', $text, $m)) {
        $facts['engines'] = ['node' => $m[1], 'bun' => $m[2], 'deno' => $m[3], 'zipp' => $m[4]];
    }
    $out['readme'] = $facts;
} else {
    $failures[] = 'readme';
}

$out['failures'] = $failures;

// Every upstream call failed: serve the last good cache if it is not ancient.
if (count($failures) >= 5 && $cached !== null) {
    $age = time() - strtotime((string) $cached['generated_at']);
    if ($age < STALE_MAX_AGE) {
        $cached['cached'] = true;
        $cached['stale'] = true;
        echo json_encode($cached, JSON_UNESCAPED_SLASHES | JSON_PRETTY_PRINT);
        exit;
    }
}

// Merge partial failures over the cache so one missing call does not blank a field.
if ($cached !== null) {
    foreach (['repo', 'release', 'commits', 'version', 'readme'] as $key) {
        if (!isset($out[$key]) && isset($cached[$key])) {
            $out[$key] = $cached[$key];
            $out['stale'] = true;
        }
    }
}

$json = json_encode($out, JSON_UNESCAPED_SLASHES | JSON_PRETTY_PRINT);
// Cache only what is worth serving again: a full or partial success. A run
// where every upstream call failed leaves the previous cache (or none) alone.
if (count($failures) < 5) {
    @file_put_contents($cacheFile, $json, LOCK_EX);
}
echo $json;
