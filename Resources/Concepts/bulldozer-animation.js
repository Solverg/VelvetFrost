/* Animated concept for the snow bulldozer. All positions are in a 384 × 256 sprite coordinate space. */
(() => {
  'use strict';

  const canvas = document.querySelector('#bulldozer');
  const context = canvas.getContext('2d');
  const image = new Image();
  const status = document.querySelector('#status');
  const pause = document.querySelector('#pause');
  const motion = window.matchMedia('(prefers-reduced-motion: reduce)');
  let paused = motion.matches;
  let elapsed = 0;
  let previous = null;

  // The canvas includes room above the sprite for exhaust, keeping the ground fixed.
  const left = 48;
  const top = 94;
  const scale = 384 / 1536;

  function pixelRect(x, y, width, height, color) {
    context.fillStyle = color;
    context.fillRect(Math.round(x), Math.round(y), width, height);
  }

  function smoke(seconds, bob) {
    // Each puff is a deterministic loop so there are no jumps after a pause.
    for (let i = 0; i < 4; i++) {
      const age = (seconds * 0.34 + i / 4) % 1;
      const x = left + 228 - age * 14 + Math.sin(age * 4 + i) * 3;
      const y = top + 72 + bob - age * 48;
      const alpha = Math.sin(age * Math.PI) * 0.38;
      const shade = `rgba(119, 135, 145, ${alpha})`;
      const size = 2 + Math.floor(age * 3);
      pixelRect(x, y, size * 3, size * 2, shade);
      pixelRect(x + size, y - size, size, size * 4, shade);
    }
  }

  // The original concept is one image. Mask the blade so it can move independently;
  // coordinates follow its silhouette in the original 1536 × 1024 image.
  function bladePath(x, y) {
    const points = [[1120, 620], [1536, 620], [1536, 1024], [1035, 1024], [1035, 772], [1090, 735]];
    context.moveTo(x + points[0][0] * scale, y + points[0][1] * scale);
    for (const [px, py] of points.slice(1)) context.lineTo(x + px * scale, y + py * scale);
    context.closePath();
  }

  function machine(seconds, bob) {
    const y = top + bob;
    const lift = Math.round((1 - Math.cos(seconds * Math.PI / 2)) * 2);

    context.save();
    context.beginPath();
    context.rect(0, 0, canvas.width, canvas.height);
    bladePath(left, y);
    context.clip('evenodd');
    context.drawImage(image, left, y, image.width * scale, image.height * scale);
    context.restore();

    context.save();
    context.beginPath();
    bladePath(left, y - lift);
    context.clip();
    context.drawImage(image, left, y - lift, image.width * scale, image.height * scale);
    context.restore();
  }

  function tracks(seconds, bob) {
    // Counter-moving marks suggest a continuous belt, with the wheels left in place.
    const step = Math.floor(seconds * 5) % 10;
    for (let x = 52; x < 212; x += 10) {
      const upper = 52 + ((x - 52 + step) % 160);
      const lower = 52 + ((x - 52 + 160 - step) % 160);
      pixelRect(left + upper, top + 174 + bob, 5, 2, 'rgba(119, 132, 146, 0.7)');
      pixelRect(left + lower, top + 239 + bob, 5, 2, 'rgba(119, 132, 146, 0.7)');
    }
  }

  function draw(seconds) {
    context.clearRect(0, 0, canvas.width, canvas.height);
    context.imageSmoothingEnabled = false;
    // Two resting positions evoke an idling engine without a distracting rapid shake.
    const bob = Math.floor(seconds * 5) % 5 === 0 ? -1 : 0;
    smoke(seconds, bob);
    machine(seconds, bob);
    tracks(seconds, bob);

    // A restrained rotating-beacon glint sits over the existing painted beacon.
    const pulse = (Math.sin(seconds * Math.PI * 2 / 1.5) + 1) / 2;
    if (pulse > 0.35) {
      const alpha = (pulse - 0.35) * 0.95;
      pixelRect(left + 125, top + 22 + bob, 6, 4, `rgba(255, 246, 185, ${alpha})`);
      pixelRect(left + 123, top + 26 + bob, 10, 3, `rgba(255, 190, 69, ${alpha * 0.6})`);
    }
  }

  function frame(now) {
    if (!paused && previous !== null) elapsed += Math.min((now - previous) / 1000, 0.1);
    previous = now;
    draw(elapsed);
    requestAnimationFrame(frame);
  }

  pause.addEventListener('click', () => {
    paused = !paused;
    pause.textContent = paused ? 'Продолжить' : 'Пауза';
    pause.setAttribute('aria-pressed', String(paused));
    previous = null;
  });
  pause.textContent = paused ? 'Продолжить' : 'Пауза';
  pause.setAttribute('aria-pressed', String(paused));
  image.onload = () => {
    status.textContent = 'Двигатель работает, дым поднимается, маячок мигает, гусеницы движутся и отвал поднимается.';
    requestAnimationFrame(frame);
  };
  image.onerror = () => { status.textContent = 'Не удалось загрузить изображение бульдозера.'; };
  image.src = 'snow-bulldozer.png';
})();
