'use strict';
/**
 * Пути артефактов E2E-прогонов.
 *
 * По умолчанию артефакты пишутся **вне репозитория** (`$XDG_STATE_HOME/vks/artifacts`,
 * иначе `~/.local/state/vks/artifacts`): прогоны весят сотни мегабайт, и держать
 * их внутри рабочего дерева — значит держать мусор в git-статусе и в сборке.
 *
 * Переопределение:
 *   VKS_ARTIFACTS_ROOT — корень для всех прогонов (каталог создаётся);
 *   VKS_ARTIFACTS      — точный каталог конкретного прогона (имеет приоритет).
 */

const fs = require('fs');
const os = require('os');
const path = require('path');

/** Корень каталога артефактов. */
function artifactsRoot() {
  if (process.env.VKS_ARTIFACTS_ROOT) {
    return path.resolve(process.env.VKS_ARTIFACTS_ROOT);
  }
  const stateHome = process.env.XDG_STATE_HOME || path.join(os.homedir(), '.local', 'state');
  return path.join(stateHome, 'vks', 'artifacts');
}

/**
 * Каталог артефактов прогона `name` (создаётся при необходимости).
 *
 * Если каталог состояния недоступен для записи (read-only FS, chroot),
 * используется локальный `artifacts/` внутри репозитория — он в `.gitignore`.
 * @param {string} name имя прогона, например `e2e-layouts`
 * @returns {string} абсолютный путь к каталогу
 */
function artifactsDir(name) {
  const candidates = process.env.VKS_ARTIFACTS
    ? [path.resolve(process.env.VKS_ARTIFACTS)]
    : [path.join(artifactsRoot(), name), path.join(__dirname, '..', '..', 'artifacts', name)];
  for (const dir of candidates) {
    try {
      fs.mkdirSync(dir, { recursive: true });
      return dir;
    } catch (error) {
      if (error.code !== 'EACCES' && error.code !== 'EROFS' && error.code !== 'ENOENT') {
        throw error;
      }
    }
  }
  return candidates[candidates.length - 1];
}

module.exports = { artifactsRoot, artifactsDir };
