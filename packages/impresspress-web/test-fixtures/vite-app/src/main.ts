import { registerWithUpdates } from 'impresspress-web';

registerWithUpdates('/sw.js').then((handle) => {
  console.log('registered', handle.registration);
});
