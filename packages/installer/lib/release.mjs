import {InstallerError} from './native.mjs';

const REPOSITORY = /^[A-Za-z0-9][A-Za-z0-9_.-]*\/[A-Za-z0-9][A-Za-z0-9_.-]*$/;

export function validateRelease(release) {
  const previous = release.previousRepositories ?? [];
  const repositories = [release.repository, ...(Array.isArray(previous) ? previous : [])];
  if (!Array.isArray(previous) || release.previousRepositories === null
      || repositories.some(repository => typeof repository !== 'string' || repository.trim() !== repository || !REPOSITORY.test(repository))
      || new Set(repositories.map(repository => repository.toLowerCase())).size !== repositories.length
      || release.url !== `https://github.com/${release.repository}.git`
      || release.marketplace !== 'delm' || release.ref !== 'marketplace' || release.plugin !== 'delm@delm') {
    throw new InstallerError('This installer has invalid release configuration. Obtain a correctly prepared package.', 'INVALID_RELEASE');
  }
}

// Repository moves are accepted only when explicitly approved in the package.
// Keep matching narrow: credentials, other protocols, query strings and extra
// path segments are never treated as an approved distribution source.
export function matchesRepository(release, repository) {
  return [release.repository, ...(release.previousRepositories ?? [])].includes(repository);
}

export function matchesRepositoryUrl(release, url) {
  return [release.repository, ...(release.previousRepositories ?? [])].some(repository =>
    url === `https://github.com/${repository}.git` || url === `https://github.com/${repository}`);
}
