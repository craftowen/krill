// Local-only D1-compatible adapter; production uses the DB binding directly.
import {DatabaseSync} from 'node:sqlite';
import {readFileSync} from 'node:fs';
export class LocalD1 {
  constructor(path = ':memory:') {
    this.sqlite = new DatabaseSync(path);
    this.sqlite.exec('PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;');
    if (this.sqlite.prepare('PRAGMA user_version').get().user_version === 0) {
      this.sqlite.exec('BEGIN');
      try {
        this.sqlite.exec(readFileSync(new URL('./migrations/0001_telemetry.sql', import.meta.url), 'utf8'));
        this.sqlite.exec('PRAGMA user_version=1; COMMIT');
      } catch (error) { this.sqlite.exec('ROLLBACK'); throw error; }
    }
  }
  prepare(sql) {
    return {sql, values: [], bind(...values) { return {...this, values}; }};
  }
  async batch(statements) {
    this.sqlite.exec('BEGIN');
    try {
      const result = statements.map(({sql, values}) => ({success:true, results:this.sqlite.prepare(sql).all(...values)}));
      this.sqlite.exec('COMMIT');
      return result;
    } catch (error) { this.sqlite.exec('ROLLBACK'); throw error; }
  }
  close() { this.sqlite.close(); }
}
