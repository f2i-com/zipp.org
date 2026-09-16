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
 * writable cache location: `api/cache/` beside this file, else a private
 * 0700 directory this process creates under the system temp directory (never
 * a predictable file shared with other local users; see cacheDirectory()).
 * One refresh runs at a time, and a failed refresh backs off for
 * FAILURE_BACKOFF seconds. Set ZIPP_GITHUB_TOKEN in the server environment to
 * lift GitHub's anonymous rate limit (60 requests/hour per address; this
 * script makes six per cache miss).
 */

const REPO = 'f2i-com/zipp.org';
const BRANCH = 'main';
const CACHE_TTL = 900;          // fresh for 15 minutes
const STALE_MAX_AGE = 86400;    // serve a failed refresh from cache up to a day
const FAILURE_BACKOFF = 60;     // after a failed refresh, wait before the next

header('Content-Type: application/json; charset=utf-8');
header('Cache-Control: no-store');
header('X-Content-Type-Options: nosniff');

/**
 * The cache directory: `api/cache/` when the deployment provides it, else a
 * directory private to this process's user under the system temp directory.
 * A fixed file name in the shared temp directory would let another local
 * user pre-create it with forged data or plant a symlink the write follows.
 * Returns null (no caching) when no private location can be established.
 */
function cacheDirectory(): ?string
{
    $local = __DIR__ . '/cache';
    if (is_dir($local) && !is_link($local) && is_writable($local)) {
        return $local;
    }
    $uid = function_exists('posix_geteuid') ? posix_geteuid() : null;
    $dir = sys_get_temp_dir() . '/zipp-landing-stats-'
        . substr(hash('sha256', __DIR__ . '|' . ($uid ?? get_current_user())), 0, 16);
    if (!is_dir($dir) && !@mkdir($dir, 0700)) {
        return null;
    }
    clearstatcache(true, $dir);
    if (is_link($dir) || !is_dir($dir) || !is_writable($dir)) {
        return null;
    }
    // Where ownership and modes are meaningful, the directory must be ours
    // and closed to everyone else, whoever created it first.
    if ($uid !== null && (fileowner($dir) !== $uid || (fileperms($dir) & 0077) !== 0)) {
        return null;
    }
    return $dir;
}

/** A cached snapshot, never read through a symlink. */
function readSnapshot(?string $file): ?array
{
    if ($file === null || is_link($file) || !is_file($file)) {
        return null;
    }
    $raw = @file_get_contents($file);
    $decoded = $raw === false ? null : json_decode($raw, true);
    return is_array($decoded) && isset($decoded['generated_at']) ? $decoded : null;
}

function snapshotAge(array $snapshot): int
{
    return time() - (int) strtotime((string) $snapshot['generated_at']);
}

function isFresh(?array $snapshot): bool
{
    if ($snapshot === null) {
        return false;
    }
    $age = snapshotAge($snapshot);
    return $age >= 0 && $age < CACHE_TTL && isset($snapshot['releases']);
}

/** Answer with a cached snapshot, or 503 when there is none recent enough. */
function serveCached(?array $snapshot, bool $stale): never
{
    if ($snapshot !== null) {
        $age = snapshotAge($snapshot);
        if ($stale ? $age >= 0 && $age < STALE_MAX_AGE : isFresh($snapshot)) {
            $snapshot['cached'] = true;
            if ($stale) {
                $snapshot['stale'] = true;
            }
            echo json_encode($snapshot, JSON_UNESCAPED_SLASHES | JSON_PRETTY_PRINT);
            exit;
        }
    }
    http_response_code(503);
    echo json_encode(['error' => 'Repository updates are temporarily unavailable.']);
    exit;
}

$cacheDir = cacheDirectory();
$cacheFile = $cacheDir === null ? null : $cacheDir . '/zipp-landing-stats-v2.json';
$failureFile = $cacheDir === null ? null : $cacheDir . '/zipp-landing-stats-v2.failed';

$cached = readSnapshot($cacheFile);
if (isFresh($cached)) {
    serveCached($cached, false);
}

// One refresh at a time: a concurrent request serves the previous snapshot
// (or waits for the refresh in progress when there is none), and a refresh
// that failed recently is not retried until FAILURE_BACKOFF has passed.
$lock = $cacheDir === null ? false : @fopen($cacheDir . '/zipp-landing-stats-v2.lock', 'c');
if ($lock !== false && !flock($lock, LOCK_EX | LOCK_NB)) {
    if ($cached !== null) {
        serveCached($cached, true);
    }
    flock($lock, LOCK_EX);
    $cached = readSnapshot($cacheFile);
    if (isFresh($cached)) {
        serveCached($cached, false);
    }
}
if ($failureFile !== null && !is_link($failureFile) && is_file($failureFile)
    && time() - (int) filemtime($failureFile) < FAILURE_BACKOFF) {
    serveCached($cached, true);
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
    if (is_string($token) && $token !== '' && str_starts_with($url, 'https://api.github.com/')) {
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
        'license' => $repo['license']['spdx_id'] ?? null,
    ];
} else {
    $failures[] = 'repo';
}

// ── latest release (tag, name, date) ────────────────────────────────────────
$releases = fetchJson($api . '/releases?per_page=10');
$latestRelease = fetchJson($api . '/releases/latest');
if ($releases !== null && array_is_list($releases)) {
    $out['releases'] = [];
    foreach ($releases as $release) {
        if (($release['draft'] ?? false) || ($release['prerelease'] ?? false)) continue;
        $out['releases'][] = [
            'tag' => $release['tag_name'],
            'name' => $release['name'] ?? $release['tag_name'],
            'published_at' => $release['published_at'] ?? null,
            'url' => $release['html_url'] ?? null,
        ];
        if (count($out['releases']) === 3) break;
    }
    if ($latestRelease !== null && isset($latestRelease['tag_name'])) {
        $out['release'] = [
            'tag' => $latestRelease['tag_name'],
            'name' => $latestRelease['name'] ?? $latestRelease['tag_name'],
            'published_at' => $latestRelease['published_at'] ?? null,
            'url' => $latestRelease['html_url'] ?? null,
        ];
    } elseif (count($out['releases']) > 0) {
        $failures[] = 'latest_release';
    } else {
        $out['release'] = null;
    }
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
    if ($latest !== null && preg_match('/^[a-f0-9]{40}$/', $latest['sha'] ?? '')) {
        $out['source']['commit'] = $latest['sha'];
        $rawBase = 'https://raw.githubusercontent.com/' . REPO . '/' . $latest['sha'] . '/';
    }
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
    // Preserve original facts and expose the explicitly corrected profile separately.
    if (preg_match('/^\| Core with five documented test corrections \|\s*([0-9,]+)\s*\|\s*([0-9,]+)\s*\|\s*([0-9,]+)\s*\|\s*$/m', $text, $m)) {
        $pass = (int) str_replace(',', '', $m[1]);
        $total = $pass + (int) str_replace(',', '', $m[2]) + (int) str_replace(',', '', $m[3]);
        if ($total > 0 && $pass <= $total && $total <= 9007199254740991) {
            $facts['test262_corrected_pass'] = $pass;
            $facts['test262_corrected_total'] = $total;
        }
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

// Publish coherent snapshots only. A partial refresh keeps the old timestamp;
// failed requests must never turn yesterday's data into a fresh success.
if (count($failures) > 0) {
    if ($failureFile !== null && !is_link($failureFile)) {
        @touch($failureFile);
    }
    serveCached($cached, true);
}

$json = json_encode($out, JSON_UNESCAPED_SLASHES | JSON_PRETTY_PRINT);
if ($cacheDir !== null) {
    // Write beside the target and rename over it: rename replaces a planted
    // symlink rather than writing through it, and readers never see a
    // partial file.
    $temp = @tempnam($cacheDir, 'stats');
    if ($temp !== false) {
        if (@file_put_contents($temp, $json) === strlen($json) && @rename($temp, $cacheFile)) {
            if (is_file($failureFile) && !is_link($failureFile)) {
                @unlink($failureFile);
            }
        } else {
            @unlink($temp);
        }
    }
}
echo $json;
