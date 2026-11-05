# frozen_string_literal: true

require_relative "test_helper"
require "socket"

class PreconnectTest < Minitest::Test
  def setup
    @server = TCPServer.new("127.0.0.1", 0)
    @connections = Queue.new
    @requests = Queue.new
    @server_thread = Thread.new do
      loop do
        conn = @server.accept
        @connections << conn
        Thread.new(conn) { |c| serve_keep_alive(c) }
      end
    rescue IOError, Errno::EBADF
    end
    @url = "http://127.0.0.1:#{@server.addr[1]}/"
  end

  def teardown
    @server.close
    @server_thread.join(2)
    @connections.size.times { @connections.pop.close rescue nil }
  end

  def test_preconnect_opens_connection_without_sending_a_request
    client = Wreq::Client.new(emulation: false, no_proxy: true)
    assert_nil client.preconnect(@url)
    wait_for { @connections.size == 1 }
    assert_equal 0, @requests.size, "preconnect must not send a request"
  end

  def test_request_reuses_preconnected_connection
    client = Wreq::Client.new(emulation: false, no_proxy: true)
    client.preconnect(@url)
    wait_for { @connections.size == 1 }

    resp = client.get(@url)
    assert_equal 200, resp.status
    assert_equal 1, @connections.size, "request should reuse the preconnected connection"
    assert_equal 1, @requests.size
  end

  def test_repeated_preconnect_reuses_idle_connection
    client = Wreq::Client.new(emulation: false, no_proxy: true)
    3.times { client.preconnect(@url) }
    client.get(@url)
    assert_equal 1, @connections.size
  end

  def test_preconnect_with_matching_proxy_option_is_reused
    client = Wreq::Client.new(emulation: false, no_proxy: true)
    proxy = @url.chomp("/")
    client.preconnect("http://example.invalid/", proxy: proxy)
    wait_for { @connections.size == 1 }

    resp = client.get("http://example.invalid/", proxy: proxy)
    assert_equal 200, resp.status
    assert_equal 1, @connections.size
    assert_equal "GET http://example.invalid/ HTTP/1.1", @requests.pop
  end

  def test_preconnect_connection_refused_raises
    port = @server.addr[1]
    @server.close
    client = Wreq::Client.new(emulation: false, no_proxy: true)
    assert_raises(Wreq::Error) { client.preconnect("http://127.0.0.1:#{port}/") }
  end

  def test_preconnect_rejects_unsupported_scheme
    client = Wreq::Client.new(emulation: false, no_proxy: true)
    assert_raises(Wreq::Error) { client.preconnect("ftp://127.0.0.1/") }
  end

  def test_preconnect_requires_url
    client = Wreq::Client.new
    assert_raises(Wreq::Error) { client.preconnect }
  end

  def test_preconnect_performs_tls_handshake_and_honors_timeout
    client = Wreq::Client.new(emulation: false, no_proxy: true)
    started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    assert_raises(Wreq::Error) do
      client.preconnect("https://127.0.0.1:#{@server.addr[1]}/", timeout: 0.3)
    end
    elapsed = Process.clock_gettime(Process::CLOCK_MONOTONIC) - started
    assert_operator elapsed, :<, 2

    wait_for { @connections.size == 1 }
    assert_equal 22, @requests.pop.getbyte(0), "expected a TLS handshake record"
  end

  def test_preconnect_https
    client = Wreq::Client.new
    client.preconnect("https://httpbun.com/")
    resp = client.get("https://httpbun.com/get")
    assert_equal 200, resp.status
  end

  private

  # Answers HTTP/1.1 requests on a persistent connection, recording each request
  # line. Non-HTTP input (e.g. a TLS ClientHello) is recorded and left unanswered.
  def serve_keep_alive(conn)
    loop do
      line = conn.gets
      break if line.nil?
      unless line.start_with?("GET ")
        @requests << line
        sleep
      end
      @requests << line.chomp
      loop do
        header = conn.gets
        break if header.nil? || header.chomp.empty?
      end
      conn.write "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok"
    end
  rescue IOError, Errno::ECONNRESET, Errno::EPIPE
  ensure
    conn.close rescue nil
  end

  def wait_for(timeout: 2)
    deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + timeout
    until yield
      flunk "condition not met within #{timeout}s" if Process.clock_gettime(Process::CLOCK_MONOTONIC) > deadline
      sleep 0.01
    end
  end
end
