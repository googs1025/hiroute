import manifest from '../../data/releases.json' with { type: 'json' };

export const RELEASE_SCHEMA = 'hiroute.website.releases/v2';
export const DOWNLOAD_ORIGIN = 'https://hiroute.ai';
const SHA256 = /^[0-9a-f]{64}$/;
const VERSION = /^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$/;
const REVISION = /^[0-9a-f]{40}$/;
const FILENAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,239}$/;
const STANDALONE_TARGETS = {
  x86_64: 'x86_64-unknown-linux-gnu',
  aarch64: 'aarch64-unknown-linux-gnu',
};

function requiredText(value, field) {
  if (typeof value !== 'string' || !value.trim()) throw new Error(`${field} must be nonempty text`);
}

export function validateReleaseManifest(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)
      || value.schema !== RELEASE_SCHEMA || !Array.isArray(value.releases)
      || Object.keys(value).some(key => !['schema', 'releases'].includes(key))) {
    throw new Error('release manifest shape is invalid');
  }
  const versions = new Set();
  let previous = Number.POSITIVE_INFINITY;
  for (const release of value.releases) {
    if (!release || typeof release !== 'object' || Array.isArray(release)
        || Object.keys(release).some(key => !['version', 'channel', 'published_at', 'notes', 'artifacts'].includes(key))
        || !VERSION.test(release.version ?? '') || !['stable', 'preview'].includes(release.channel)
        || !release.notes || typeof release.notes !== 'object'
        || !Array.isArray(release.artifacts) || release.artifacts.length === 0) {
      throw new Error(`invalid release record: ${release?.version ?? 'unknown'}`);
    }
    requiredText(release.notes.zh, 'notes.zh'); requiredText(release.notes.en, 'notes.en');
    const published = Date.parse(release.published_at);
    if (!Number.isFinite(published) || published > previous) throw new Error('releases must use valid newest-first dates');
    previous = published;
    if (versions.has(release.version)) throw new Error(`duplicate release version: ${release.version}`);
    versions.add(release.version);
    const filenames = new Set();
    for (const artifact of release.artifacts) {
      const commonValid = artifact && typeof artifact === 'object' && !Array.isArray(artifact)
        && FILENAME.test(artifact.filename ?? '') && SHA256.test(artifact.sha256 ?? '')
        && Number.isSafeInteger(artifact.size) && artifact.size > 0;
      const desktopValid = commonValid && artifact.kind === 'desktop'
        && !Object.keys(artifact).some(key => !['kind', 'platform', 'architecture', 'format', 'minimum_os', 'distribution', 'filename', 'sha256', 'size'].includes(key))
        && artifact.platform === 'macOS' && ['arm64', 'x86_64'].includes(artifact.architecture)
        && artifact.format === 'dmg' && ['self-signed', 'developer-id'].includes(artifact.distribution)
        && artifact.minimum_os === '15.0';
      const standaloneValid = commonValid && artifact.kind === 'standalone'
        && !Object.keys(artifact).some(key => !['kind', 'platform', 'architecture', 'target', 'format', 'distribution', 'filename', 'sha256', 'size', 'manifest_filename', 'manifest_sha256', 'manifest_size'].includes(key))
        && artifact.platform === 'Linux' && artifact.target === STANDALONE_TARGETS[artifact.architecture]
        && artifact.format === 'tar.gz' && artifact.distribution === 'unsigned'
        && artifact.manifest_filename === `${artifact.filename}.json`
        && FILENAME.test(artifact.manifest_filename ?? '') && SHA256.test(artifact.manifest_sha256 ?? '')
        && Number.isSafeInteger(artifact.manifest_size) && artifact.manifest_size > 0;
      if (!desktopValid && !standaloneValid) {
        throw new Error(`invalid public artifact in ${release.version}`);
      }
      if (desktopValid) {
        const suffix = artifact.distribution === 'self-signed' ? 'trial' : 'developer-id';
        const expected = new RegExp(`^HiRoute-${release.version.replaceAll('.', '\\.')}-[0-9a-f]{12}-macos-${artifact.architecture}-${suffix}\\.dmg$`);
        if (!expected.test(artifact.filename)) throw new Error(`artifact filename does not match release: ${artifact.filename}`);
      }
      if (standaloneValid) {
        const expected = new RegExp(`^hiroute-${release.version.replaceAll('.', '\\.')}-[0-9a-f]{12}-${artifact.target}\\.tar\\.gz$`);
        if (!expected.test(artifact.filename)) throw new Error(`artifact filename does not match release: ${artifact.filename}`);
      }
      if (filenames.has(artifact.filename)) throw new Error(`duplicate artifact: ${artifact.filename}`);
      filenames.add(artifact.filename);
      if (standaloneValid) {
        if (filenames.has(artifact.manifest_filename)) throw new Error(`duplicate artifact: ${artifact.manifest_filename}`);
        filenames.add(artifact.manifest_filename);
      }
    }
  }
  return value;
}

export function artifactKey(release, artifact) {
  return `releases/${release.version}/${artifact.filename}`;
}

export function artifactUrl(release, artifact) {
  return `${DOWNLOAD_ORIGIN}/${artifactKey(release, artifact)}`;
}

export function artifactManifestKey(release, artifact) {
  if (artifact.kind !== 'standalone') throw new Error('only standalone artifacts have package manifests');
  return `releases/${release.version}/${artifact.manifest_filename}`;
}

export function artifactManifestUrl(release, artifact) {
  return `${DOWNLOAD_ORIGIN}/${artifactManifestKey(release, artifact)}`;
}

export function artifactAssets(release, artifact) {
  const assets = [{
    filename: artifact.filename,
    key: artifactKey(release, artifact),
    sha256: artifact.sha256,
    size: artifact.size,
  }];
  if (artifact.kind === 'standalone') assets.push({
    filename: artifact.manifest_filename,
    key: artifactManifestKey(release, artifact),
    sha256: artifact.manifest_sha256,
    size: artifact.manifest_size,
  });
  return assets;
}

export function verifyDesktopIdentity(release, artifact, identity, expectedRevision) {
  if (artifact.kind !== 'desktop' || !REVISION.test(expectedRevision ?? '')) {
    throw new Error('desktop identity verification inputs are invalid');
  }
  const expectedDistribution = artifact.distribution === 'self-signed' ? 'controlled-trial' : 'developer-id';
  const expected = {
    version: release.version,
    revision: expectedRevision,
    architecture: artifact.architecture,
    distribution: expectedDistribution,
    dmg_sha256: artifact.sha256,
    integrity: 'green',
    mounted_components: 'green',
    detach: 'green',
  };
  if (!identity || typeof identity !== 'object' || Array.isArray(identity)
      || Object.entries(expected).some(([key, value]) => identity[key] !== value)) {
    throw new Error(`desktop package identity differs from release record: ${artifact.filename}`);
  }
  const suffix = artifact.distribution === 'self-signed' ? 'trial' : 'developer-id';
  const expectedFilename = `HiRoute-${release.version}-${expectedRevision.slice(0, 12)}-macos-${artifact.architecture}-${suffix}.dmg`;
  if (artifact.filename !== expectedFilename) {
    throw new Error(`desktop artifact filename differs from release tag: ${artifact.filename}`);
  }
  return identity;
}

export function latestRelease(channel = 'stable', value = manifest) {
  return validateReleaseManifest(value).releases.find(release => release.channel === channel) ?? null;
}

export const releaseManifest = validateReleaseManifest(manifest);

export function formatBytes(bytes, language = 'zh') {
  const units = language === 'zh' ? ['B', 'KB', 'MB', 'GB'] : ['B', 'KB', 'MB', 'GB'];
  let value = bytes, index = 0;
  while (value >= 1024 && index < units.length - 1) { value /= 1024; index += 1; }
  return `${value.toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
}
