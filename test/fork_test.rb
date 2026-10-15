# frozen_string_literal: true

require_relative "test_helper"
require "socket"

class ForkTest < Minitest::Test
  def setup
    skip "fork is not available" unless Process.respond_to?(:fork)
    @server = TCPServer.new("127.0.0.1", 0)
    @url = "http://127.0.0.1:#{@server.addr[1]}/"
    @server_thread = Thread.new { loop { serve(@server.accept) } }
  end

  def teardown
    @server_thread&.kill
    @server&.close
  end

  def test_new_client_works_in_child_after_parent_used_the_runtime
    assert_equal 200, new_client.get(@url).status

    assert_equal "200", in_child { new_client.get(@url).status.to_s }
  end

  def test_unused_client_from_parent_works_in_child
    client = new_client

    assert_equal "200", in_child { client.get(@url).status.to_s }
  end

  def test_used_client_from_parent_raises_in_child
    client = new_client
    assert_equal 200, client.get(@url).status

    message = in_child do
      client.get(@url)
      "no error"
    rescue Wreq::Error => e
      e.message
    end

    assert_match(/cannot be reused after fork/, message)
    assert_equal 200, client.get(@url).status
  end

  def test_used_client_from_parent_raises_on_batch_in_child
    client = new_client
    assert_equal 200, client.get(@url).status

    message = in_child do
      client.request_batch([@url])
      "no error"
    rescue Wreq::Error => e
      e.message
    end

    assert_match(/cannot be reused after fork/, message)
  end

  private

  def new_client
    Wreq::Client.new(emulation: false, http1_only: true, no_proxy: true, timeout: 5)
  end

  # Keep-alive server so the parent's pool holds an idle connection across fork.
  def serve(connection)
    Thread.new do
      while connection.gets("\r\n\r\n")
        connection.write("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
      end
    rescue IOError, SystemCallError
      nil
    ensure
      connection.close
    end
  end

  def in_child
    reader, writer = IO.pipe
    pid = fork do
      reader.close
      result = begin
        yield
      rescue Exception => e # rubocop:disable Lint/RescueException
        "#{e.class}: #{e.message}"
      end
      writer.write(result)
      writer.close
      exit!(0)
    end
    writer.close
    deadline = Time.now + 15
    sleep 0.05 until Process.waitpid(pid, Process::WNOHANG) || Time.now > deadline
    if Time.now > deadline
      Process.kill(:KILL, pid)
      Process.wait(pid)
      flunk "child process hung"
    end
    reader.read
  ensure
    reader&.close
  end
end
