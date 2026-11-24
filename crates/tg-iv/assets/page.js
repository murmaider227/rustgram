/*
 * Instant View WebView bridge and interactive controls for Rustgram (GPL-3.0-or-later).
 * Slideshow behavior ported from Telegram Desktop's Resources/export_rich/js/rich_message.js.
 */
(function () {
	'use strict';

	function postToHost(payload) {
		var text = typeof payload === 'string' ? payload : JSON.stringify(payload);
		try {
			if (window.chrome && window.chrome.webview && typeof window.chrome.webview.postMessage === 'function') {
				window.chrome.webview.postMessage(text);
				return;
			}
			if (window.external && typeof window.external.invoke === 'function') {
				window.external.invoke(text);
				return;
			}
			if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.tgIv) {
				window.webkit.messageHandlers.tgIv.postMessage(text);
			}
		} catch (_) {
		}
	}

	window.TelegramWebviewProxy = {
		postEvent: function (eventType, eventData) {
			postToHost([eventType, eventData]);
		}
	};

	function parseMediaAttr(el) {
		var attr = el.getAttribute('data-iv-media');
		if (!attr) {
			return null;
		}
		var idx = attr.indexOf(':');
		if (idx <= 0) {
			return null;
		}
		var id = parseInt(attr.slice(idx + 1), 10);
		if (!isFinite(id)) {
			return null;
		}
		return { kind: attr.slice(0, idx), id: id };
	}

	document.addEventListener('click', function (ev) {
		var target = ev.target;
		if (!target || !target.closest) {
			return;
		}

		var spoiler = target.closest('tg-spoiler, img[tg-spoiler], video[tg-spoiler]');
		if (spoiler && !spoiler.classList.contains('revealed')) {
			spoiler.classList.add('revealed');
			ev.preventDefault();
			return;
		}

		var joinBtn = target.closest('[data-iv-channel-join]');
		if (joinBtn) {
			ev.preventDefault();
			var cid = parseInt(joinBtn.getAttribute('data-iv-channel-join'), 10);
			if (isFinite(cid)) {
				postToHost({
					type: 'channel_join',
					channel_id: cid,
					username: joinBtn.getAttribute('data-channel-username') || null
				});
			}
			return;
		}

		var openCh = target.closest('[data-iv-channel-open]');
		if (openCh) {
			ev.preventDefault();
			var openId = parseInt(openCh.getAttribute('data-iv-channel-open'), 10);
			if (isFinite(openId)) {
				postToHost({
					type: 'channel_open',
					channel_id: openId,
					username: openCh.getAttribute('data-channel-username') || null
				});
			}
			return;
		}

		var reportEl = target.closest('[data-iv-report]');
		if (reportEl) {
			ev.preventDefault();
			var pageId = parseInt(reportEl.getAttribute('data-iv-report'), 10);
			if (isFinite(pageId)) {
				postToHost({ type: 'report', webpage_id: pageId });
			}
			return;
		}

		var link = target.closest('a[href]');
		if (link) {
			var href = link.getAttribute('href') || '';
			var wpAttr = link.getAttribute('data-webpage-id');
			var wpId = wpAttr ? parseInt(wpAttr, 10) : null;
			if (wpId !== null && !isFinite(wpId)) {
				wpId = null;
			}
			ev.preventDefault();
			postToHost({
				type: 'link_click',
				href: href,
				webpage_id: wpId,
				new_window: !!(ev.ctrlKey || ev.metaKey || ev.button === 1)
			});
			return;
		}

		var mediaEl = target.closest('[data-iv-media]');
		if (mediaEl) {
			var info = parseMediaAttr(mediaEl);
			if (info) {
				ev.preventDefault();
				postToHost({
					type: 'media_click',
					kind: info.kind,
					id: info.id
				});
			}
		}
	});

	document.addEventListener('contextmenu', function (ev) {
		ev.preventDefault();
		var target = ev.target && ev.target.closest ? ev.target : null;
		var link = target ? target.closest('a[href]') : null;
		var mediaEl = target ? target.closest('[data-iv-media]') : null;
		var mediaInfo = mediaEl ? parseMediaAttr(mediaEl) : null;
		var selection = '';
		try {
			selection = String(window.getSelection ? window.getSelection() : '');
		} catch (_) {
		}
		postToHost({
			type: 'context_menu',
			x: ev.clientX,
			y: ev.clientY,
			selection: selection,
			link_url: link ? link.getAttribute('href') : null,
			media_kind: mediaInfo ? mediaInfo.kind : null,
			media_id: mediaInfo ? mediaInfo.id : null
		});
	});

	var scrollTimer = null;
	window.addEventListener('scroll', function () {
		if (scrollTimer !== null) {
			return;
		}
		scrollTimer = window.setTimeout(function () {
			scrollTimer = null;
			var doc = document.documentElement;
			var y = window.scrollY || doc.scrollTop || 0;
			var maxY = Math.max(0, (doc.scrollHeight || 0) - (window.innerHeight || 0));
			var progress = maxY > 0 ? Math.min(1, Math.max(0, y / maxY)) : 0;
			postToHost({
				type: 'scroll_position',
				y: y,
				max_y: maxY,
				progress: progress
			});
		}, 120);
	}, { passive: true });

	function scrollToAnchor(fragment) {
		if (!fragment) {
			return false;
		}
		var target = document.getElementById(fragment);
		if (!target) {
			var anchors = document.getElementsByName(fragment);
			if (anchors && anchors.length) {
				target = anchors[0];
			}
		}
		if (!target) {
			return false;
		}
		var parent = target.parentElement;
		while (parent) {
			if (parent.tagName === 'DETAILS') {
				parent.open = true;
			}
			parent = parent.parentElement;
		}
		target.scrollIntoView({ behavior: 'smooth', block: 'start' });
		return true;
	}

	window.IvBridge = {
		dispatch: function (cmd) {
			if (!cmd || !cmd.type) {
				return;
			}
			switch (cmd.type) {
			case 'scroll_to_anchor':
				scrollToAnchor(cmd.fragment);
				break;
			case 'scroll_to_y':
				window.scrollTo(0, Number(cmd.y) || 0);
				break;
			case 'set_zoom':
				document.documentElement.style.zoom = (Number(cmd.percent) || 100) + '%';
				break;
			case 'set_theme':
				document.documentElement.setAttribute('data-theme', cmd.dark ? 'dark' : 'light');
				break;
			case 'update_media':
				document.querySelectorAll('[data-iv-media="' + cmd.kind + ':' + cmd.id + '"]').forEach(function (el) {
					if (el.tagName === 'IMG' || el.tagName === 'VIDEO' || el.tagName === 'AUDIO') {
						el.setAttribute('src', cmd.url);
					}
				});
				break;
			case 'set_channel_joined':
				document.querySelectorAll('[data-iv-channel-join="' + cmd.channel_id + '"]').forEach(function (btn) {
					btn.classList.toggle('joined', !!cmd.joined);
					btn.textContent = cmd.joined ? 'Joined' : 'Join';
				});
				break;
			case 'history_back':
				window.history.back();
				break;
			case 'history_forward':
				window.history.forward();
				break;
			}
		}
	};

	document.querySelectorAll('tg-slideshow').forEach(function (root) {
		var track = root.querySelector('.slideshow-track');
		if (!track || !track.children.length) {
			return;
		}
		var slides = track.children.length;
		var dots = Array.prototype.slice.call(root.querySelectorAll('.slideshow-dot'));
		var index = 0;
		var show = function (next) {
			index = ((next % slides) + slides) % slides;
			track.style.transform = 'translateX(' + (-index * 100) + '%)';
			dots.forEach(function (dot, i) {
				dot.classList.toggle('active', i === index);
			});
			Array.prototype.forEach.call(track.children, function (slide, i) {
				slide.querySelectorAll('video').forEach(function (video) {
					if (i === index) {
						if (video.autoplay && video.paused) {
							video.play().catch(function () {});
						}
					} else if (!video.paused) {
						video.pause();
					}
				});
			});
		};
		var prev = root.querySelector('.slideshow-prev');
		var next = root.querySelector('.slideshow-next');
		if (prev) {
			prev.addEventListener('click', function () { show(index - 1); });
		}
		if (next) {
			next.addEventListener('click', function () { show(index + 1); });
		}
		dots.forEach(function (dot, i) {
			dot.addEventListener('click', function () { show(i); });
		});
		show(0);
	});
})();
