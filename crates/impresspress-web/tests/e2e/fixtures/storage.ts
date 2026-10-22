import { expect, request as playwrightRequest } from '@playwright/test';
import { adminBearer } from './auth';

/** A bucket holding `keys` (tiny text files), made through the API. */
export async function seedBucket(baseURL: string | undefined, keys: string[]): Promise<{
  bucket: string;
  share: (key: string) => Promise<string>;
  revoke: (id: string) => Promise<void>;
  remove: (key: string) => Promise<void>;
  drop: () => Promise<void>;
}> {
  const api = await playwrightRequest.newContext({ baseURL, storageState: { cookies: [], origins: [] } });
  const Authorization = await adminBearer(api);
  const bucket = `e2e-bulk-${Date.now().toString(36)}`;
  const made = await api.post('/b/storage/api/buckets', {
    headers: { Authorization, 'Content-Type': 'application/json' },
    data: { name: bucket, public: false },
  });
  expect(made.status(), await made.text()).toBe(200);
  for (const key of keys) {
    const up = await api.post(`/b/storage/api/buckets/${bucket}/objects?key=${encodeURIComponent(key)}`, {
      headers: { Authorization, 'Content-Type': 'text/plain' },
      data: Buffer.from(key),
    });
    expect(up.status(), await up.text()).toBe(200);
  }
  return {
    bucket,
    share: async (key) => {
      const made = await api.post('/b/cloudstorage/shares', {
        headers: { Authorization, 'Content-Type': 'application/json' },
        data: { bucket, key, expires_in_hours: 24 },
      });
      expect(made.status(), await made.text()).toBe(200);
      return (await made.json()).id as string;
    },
    revoke: async (id) => {
      const gone = await api.delete(`/b/cloudstorage/shares/${encodeURIComponent(id)}`, { headers: { Authorization } });
      expect(gone.ok(), await gone.text()).toBe(true);
    },
    remove: async (key) => {
      const gone = await api.delete(`/b/storage/api/buckets/${bucket}/objects/${encodeURIComponent(key)}`, {
        headers: { Authorization },
      });
      expect(gone.ok(), await gone.text()).toBe(true);
    },
    drop: async () => {
      const gone = await api.delete(`/b/storage/api/buckets/${bucket}`, { headers: { Authorization } });
      expect(gone.ok(), await gone.text()).toBe(true);
    },
  };
}
