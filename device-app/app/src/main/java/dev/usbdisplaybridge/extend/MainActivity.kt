package dev.usbdisplaybridge.extend

import android.app.Activity
import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Bundle
import android.util.Log
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.WindowManager
import java.io.DataInputStream
import java.io.EOFException
import java.net.Socket

private const val TAG = "ExtendMode"

// Must match the host's --port default -- see docs/protocol.md's note on
// this being a known rough edge (not passed via intent extra yet).
private const val PORT = 27183
private const val PROTOCOL_VERSION = 1

/**
 * Extend mode's phone-side receiver: connects back to the host over the
 * adb reverse tunnel, decodes the H.264 stream (docs/protocol.md), and
 * renders it fullscreen. No UI beyond the SurfaceView -- this app has
 * exactly one job.
 */
class MainActivity : Activity(), SurfaceHolder.Callback {

    private lateinit var surfaceView: SurfaceView

    @Volatile
    private var running = false
    private var receiveThread: Thread? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // A monitor that falls asleep mid-session isn't useful.
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        hideSystemBars()

        surfaceView = SurfaceView(this)
        setContentView(surfaceView)
        surfaceView.holder.addCallback(this)
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) hideSystemBars()
    }

    // Legacy systemUiVisibility flags rather than WindowInsetsController:
    // still functional on every API level this app targets (26+), and
    // this is the only place fullscreen handling is needed, so it isn't
    // worth an androidx.core dependency just to use the newer API.
    @Suppress("DEPRECATION")
    private fun hideSystemBars() {
        window.decorView.systemUiVisibility = (
            View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
                or View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                or View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                or View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                or View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                or View.SYSTEM_UI_FLAG_FULLSCREEN
            )
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        running = true
        receiveThread = Thread { runDecodeLoop(holder) }.also { it.start() }
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        running = false
        receiveThread?.interrupt()
    }

    /**
     * Connects, checks the protocol version, then alternates reading one
     * framed access unit and feeding it to the decoder. Runs entirely on
     * its own thread; a synchronous dequeue/queue loop is used instead of
     * MediaCodec's async callback API since this thread has nothing else
     * to do while waiting -- one less layer of callback-thread bookkeeping
     * to get right for a first version.
     */
    private fun runDecodeLoop(holder: SurfaceHolder) {
        var codec: MediaCodec? = null
        try {
            Socket("127.0.0.1", PORT).use { socket ->
                socket.tcpNoDelay = true
                val input = DataInputStream(socket.getInputStream())

                val version = input.readUnsignedByte()
                if (version != PROTOCOL_VERSION) {
                    Log.e(TAG, "protocol version mismatch: host=$version device=$PROTOCOL_VERSION")
                    return
                }

                // Configured with a placeholder size -- the decoder adapts
                // to the real dimensions once it sees the first in-stream
                // SPS. The host encodes with x264-params=repeat-headers=1,
                // so SPS/PPS precede every IDR rather than just the very
                // first one (see docs/protocol.md); MediaCodec's own AVC
                // parser picks these up from plain input buffers, so no
                // separate CSD extraction/configure step is needed here.
                val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, 1920, 1080)
                codec = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC).apply {
                    configure(format, holder.surface, null, 0)
                    start()
                }

                while (running) {
                    val length = input.readInt()
                    val payload = ByteArray(length)
                    input.readFully(payload)
                    feedInput(codec!!, payload)
                    drainOutput(codec!!)
                }
            }
        } catch (e: EOFException) {
            Log.i(TAG, "host closed the connection")
        } catch (e: Exception) {
            Log.e(TAG, "decode loop failed", e)
        } finally {
            codec?.let {
                runCatching { it.stop() }
                runCatching { it.release() }
            }
            runOnUiThread { finish() }
        }
    }

    private fun feedInput(codec: MediaCodec, payload: ByteArray) {
        val index = codec.dequeueInputBuffer(-1) // block until one is free
        val buffer = codec.getInputBuffer(index) ?: return
        buffer.clear()
        buffer.put(payload)
        codec.queueInputBuffer(index, 0, payload.size, System.nanoTime() / 1000, 0)
    }

    private fun drainOutput(codec: MediaCodec) {
        val info = MediaCodec.BufferInfo()
        while (true) {
            val index = codec.dequeueOutputBuffer(info, 0)
            if (index < 0) break
            codec.releaseOutputBuffer(index, true)
        }
    }
}
