export const updateLabels = {
  idle: '업데이트 확인', checking: '확인 중', installing: '업데이트 진행 중',
  installed: '업데이트 완료', latest: '최신 버전', error: '다시 확인',
} as const;
type UpdateResult = { status: 'latest' | 'installed'; version: string };
export class AppUpdate {
  version: string;
  private perform: (progress: () => void) => Promise<UpdateResult>;
  private changed: () => void;
  phase: keyof typeof updateLabels;
  error = '';
  constructor(version: string, completed: boolean,
    perform: (progress: () => void) => Promise<UpdateResult>, changed: () => void) {
    this.version = version; this.perform = perform; this.changed = changed;
    this.phase = completed ? 'installed' : 'idle';
  }
  get busy() { return this.phase === 'checking' || this.phase === 'installing'; }
  get label() { return updateLabels[this.phase]; }
  async check() {
    if (this.busy) return;
    this.error = ''; this.phase = 'checking'; this.changed();
    try {
      const result = await this.perform(() => { this.phase = 'installing'; this.changed(); });
      this.version = result.version; this.phase = result.status;
    } catch (error) {
      this.phase = 'error';
      this.error = error instanceof Error ? error.message : String(error);
    }
    this.changed();
  }
}
