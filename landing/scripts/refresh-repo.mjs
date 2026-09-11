import { writeFile } from 'node:fs/promises'
import { fetchRepository } from '../worker/repository.js'

// Optional offline-snapshot refresh; normal builds do not depend on GitHub.
const snapshot = await fetchRepository()
await writeFile(new URL('../src/repo-snapshot.json', import.meta.url), JSON.stringify(snapshot, null, 2) + '\n')
console.log(`Saved repository snapshot: ${snapshot.release?.tag ?? 'no stable release'}, ${snapshot.generated_at}`)
