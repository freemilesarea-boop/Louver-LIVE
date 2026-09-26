/**
 * The desktop adapter, against the desktop's own mock backend.
 *
 * This is what makes the seam real rather than decorative: the cloud screens'
 * transport can be satisfied by the Rust commands the desktop app already has.
 */
import { describe, expect, it } from 'vitest'
import { setMockBackend } from '@/services/ipc'
import { DesktopTransport } from './desktopTransport'

function backend(over: Record<string, unknown> = {}) {
  const answers: Record<string, unknown> = {
    list_playlists: [
      { id: 7, name: '밤 라디오', playback_mode: 'sequential', output_profile: 'p1080p30', created_at: '', updated_at: '' },
      { id: 8, name: '낮 라디오', playback_mode: 'sequential', output_profile: 'p1080p30', created_at: '', updated_at: '' },
    ],
    get_status: {
      supervisor: { state: 'LIVE', restart_count: 1 },
      playlist_id: 7,
      playlist_name: '밤 라디오',
      item_count: 1,
      elapsed_secs: 120,
      dry_run: false,
      cycle_duration_secs: 5340,
    },
    list_media: [
      {
        id: 3, source_path: '/v/a.mp4', display_name: 'a.mp4', status: 'normalized', media_hash: '',
        duration_secs: 5340, width: 1920, height: 1080, fps: 30, video_codec: 'h264',
        audio_codec: 'aac', is_hdr: false, file_size: 900, added_at: '',
      },
    ],
    ...over,
  }
  setMockBackend((cmd) => {
    if (!(cmd in answers)) throw new Error(`unexpected command ${cmd}`)
    return answers[cmd]
  })
}

describe('DesktopTransport', () => {
  it('presents one machine as one slot, with the live playlist running', async () => {
    backend()
    const d = await new DesktopTransport().dashboard()

    expect(d.allowed).toBe(1)
    expect(d.active).toBe(1)
    expect(d.plan_label).toBe('데스크톱')
    expect(d.broadcasts.map((b) => [b.name, b.runtime_state, b.desired_state])).toEqual([
      ['밤 라디오', 'RUNNING', 'running'],
      ['낮 라디오', 'CREATED', 'stopped'],
    ])
    expect(d.broadcasts[0]?.restart_count).toBe(1)
  })

  it('maps the desktop’s media states onto the cloud’s', async () => {
    backend()
    const [m] = await new DesktopTransport().listMedia()
    expect(m?.state).toBe('ready')
    expect(m?.filename).toBe('a.mp4')
  })

  it('has one destination, masked, and never a key', async () => {
    const [d] = await new DesktopTransport().listDestinations()
    expect(d?.key_masked).toBe('••••••••••••')
    expect(d && 'key' in d).toBe(false)
  })

  it('says plainly what the desktop cannot do, rather than pretending', async () => {
    const t = new DesktopTransport()
    await expect(t.login()).rejects.toThrow('웹 버전에서만')
    await expect(t.uploadMedia()).rejects.toThrow('웹 버전에서만')
    await expect(t.createDestination({ label: '', rtmps_url: '', stream_key: 'x' })).rejects.toThrow(
      '웹 버전에서만',
    )
  })
})
