create table object_detection_model (
  id integer primary key,
  uuid blob unique not null check (length(uuid) = 16),
  name text not null,

  -- The actual model and label mappings, in a tbd protocol buffer message
  -- format.
  data blob
);

insert into object_detection_model (id, uuid, name)
    values (1, X'02054A3862CF42FF9FFA04876A2970D0', 'MobileNet SSD v2 (COCO)');

create table object_detection_label (
  id integer primary key,
  uuid blob unique not null check (length(uuid) = 16),
  name text unique not null,
  color text
);

insert into object_detection_label (id, uuid, name, color)
    values ( 0, X'6C1C92BFD52547A6A251AD382B2329E8', 'person', 'red'),
           ( 1, X'49FAE8BE47654CA592EE50C6836308D1', 'bicycle', 'black'),
           ( 2, X'D5B5783ED788414D8902733D7E968E2B', 'car', 'black'),
           ( 3, X'9F65502CE04742A797331784FDA56C11', 'motorcycle', 'black'),
           ( 4, X'9734FAFF706A48B5AFEE4DCBCA008589', 'airplane', 'black'),
           ( 5, X'6EA2B36F57FD48BEA869F16D34825AE6', 'bus', 'black'),
           ( 6, X'CF6178490D414BF28E68480E1D3C2ACF', 'train', 'black'),
           ( 7, X'713C35D1885843079F306758429120DE', 'truck', 'black'),
           ( 8, X'DCF944379D304BFF980A9C128BE840D8', 'boat', 'black'),
           ( 9, X'AF1B92D6A7B448DFAD0B33C860D2BD33', 'traffic light', 'black'),
           (10, X'667E7B219D914DD6A167CCB90A797887', 'fire hydrant', 'black'),
           (12, X'7F70CE0FA9FF49D4838083CB80422182', 'stop sign', 'black'),
           (13, X'27FD1CE67F88439CB1EA6687C7B34A2A', 'parking meter', 'black'),
           (14, X'EAC5FB1015D3499690ACE2D3F3730C3E', 'bench', 'black'),
           (15, X'72A440E29D12426DB2D59773D0D5FD2B', 'bird', 'black'),
           (16, X'F676BB22707947B5BC5B266E1C08133E', 'cat', 'black'),
           (17, X'3818BDAFAC5F4B16A60D53F795057CFE', 'dog', 'black'),
           (18, X'548590A030A542CE8DC8EE8684BBFFA5', 'horse', 'black'),
           (19, X'1B21769E8BB24F9BB97919314C32DEB1', 'sheep', 'black'),
           (20, X'6BDF42D1E05A499D97EB37061C20F5DD', 'cow', 'black'),
           (21, X'73E02CA97D384B32A7CBDCF574558789', 'elephant', 'black'),
           (22, X'E5DD9BC5BAB743D58628D4C64A3B2100', 'bear', 'black'),
           (23, X'85653F8341CC4DE0B129925637DBC238', 'zebra', 'black'),
           (24, X'971791FE616142C596F25430DD404FAC', 'giraffe', 'black'),
           (26, X'BBFA879319A144B695781E735B5460DE', 'backpack', 'black'),
           (27, X'4F817EAD58E446A4BDA6EE5B8BE644EF', 'umbrella', 'black'),
           (30, X'60934310DCB446B698C23C33818B8F67', 'handbag', 'black'),
           (31, X'C47133F0637D4953ADB91F619F780243', 'tie', 'black'),
           (32, X'ECFD88A73E4C4F90A485B2377F39E595', 'suitcase', 'black'),
           (33, X'FDE6854939FD4C10A434AABB4951A18F', 'frisbee', 'black'),
           (34, X'50649D276D33460DB4F6A79799D3C10A', 'skis', 'black'),
           (35, X'0149596FF68440DAB15208ABA67B0115', 'snowboard', 'black'),
           (36, X'75FED0E8E4384363B8BFB80F7CB2A00B', 'sports ball', 'black'),
           (37, X'C614D693C6EF47E1A4B934D0D530CE2B', 'kite', 'black'),
           (38, X'C8C459C5E3A34338BFA93D83610814DF', 'baseball bat', 'black'),
           (39, X'D82E77765E0F4AD3BFD28F47149F29B2', 'baseball glove', 'black'),
           (40, X'6FA9977FB74D4B1980509497CDBA7C5B', 'skateboard', 'black'),
           (41, X'3B1D185DF5EA4410B0A73757810B61A8', 'surfboard', 'black'),
           (42, X'D0F4DEC513744AD0904E98E852AE243A', 'tennis racket', 'black'),
           (43, X'B28C42FD0B644CDE93CD937D544BBA91', 'bottle', 'black'),
           (45, X'F5EC83EC0FDC44D6A46717DCA0843F16', 'wine glass', 'black'),
           (46, X'9A4B6FDE8E6B4F3980E1DFBE841CB68B', 'cup', 'black'),
           (47, X'1C754958E938413D8518122D75D64567', 'fork', 'black'),
           (48, X'F5E797073CF64F76A0A847710D0D793D', 'knife', 'black'),
           (49, X'04131434888943DAA45ADCB8AC2F0003', 'spoon', 'black'),
           (50, X'BD8DC2AFF0B7466DB95457B2C4AED574', 'bowl', 'black'),
           (51, X'8EA055CCCE8F42ECB6AB3C93961BC4D4', 'banana', 'black'),
           (52, X'93DF007C47B945B9B0C9EC111A3B0980', 'apple', 'black'),
           (53, X'B32D85397431406C8DE7E1A8B1D0F4D7', 'sandwich', 'black'),
           (54, X'7BF75090452D463DA6F58D849476AEF9', 'orange', 'black'),
           (55, X'122B10F8D5BF43928AFD2F3A6626D32E', 'broccoli', 'black'),
           (56, X'30C0C65ED584470CBA4D95333D7E8C27', 'carrot', 'black'),
           (57, X'7728C4D0856E4A03BDE72AFDFD2FFCD5', 'hot dog', 'black'),
           (58, X'4355C1DDBAB54FE09780BD5E1AC5337D', 'pizza', 'black'),
           (59, X'7261CFE662A94555B19F72B8534C0F69', 'donut', 'black'),
           (60, X'F7326BF841D9405486347C987865210C', 'cake', 'black'),
           (61, X'D212A0AA91E44B21B7D08DD4BC26BFF6', 'chair', 'black'),
           (62, X'205FA3D9E1664F13B19012FD1FAD80B2', 'couch', 'black'),
           (63, X'E2146568E7F04E858AF7FF194BBEA53A', 'potted plant', 'black'),
           (64, X'B62E011FACEB4C12A6FC3FB18BDF0D15', 'bed', 'black'),
           (66, X'EA59369372CE419A90655D0BDAF457F5', 'dining table', 'black'),
           (69, X'5C4D7A1FD39248298B0EA517FD4DCB81', 'toilet', 'black'),
           (71, X'68951C45C44747D18662493979F1C2F3', 'tv', 'black'),
           (72, X'7BFE1F9F85CF4EE882C8BC5818794B44', 'laptop', 'black'),
           (73, X'3A27E2B8CEAA4F32BC7E29836917BD84', 'mouse', 'black'),
           (74, X'F6D3F2DC6B3F4946B56EF997F845880D', 'remote', 'black'),
           (75, X'FE16DC0BEE1C4C919D74E4958588827E', 'keyboard', 'black'),
           (76, X'B4793A0B77704F6D9B4CB0C0DF98EE10', 'cell phone', 'black'),
           (77, X'E973311E85384D83B4BDC7A087A6A1BB', 'microwave', 'black'),
           (78, X'9DAFE875BCBA460A8863CDDB40B2FFBB', 'oven', 'black'),
           (79, X'00D3D1B1B40647198DBD84DE5AD68BAA', 'toaster', 'black'),
           (80, X'AFB39111EEED4FB99175F190011C6FB3', 'sink', 'black'),
           (81, X'BE2B4967C2D843949193CDFA9146E7B9', 'refrigerator', 'black'),
           (83, X'07325ACE3F214D2AB6AE6EF0EC2CBDD0', 'book', 'black'),
           (84, X'8E8E7C4C84414DF1900FC9D9DF641A48', 'clock', 'black'),
           (85, X'D9CE40BF7B6444139DEACCABC05597EE', 'vase', 'black'),
           (86, X'EF180FC3455144E6AA0C5360483C6B37', 'scissors', 'black'),
           (87, X'D82F948297E14E8B8C6FD0349A6F1D8A', 'teddy bear', 'black'),
           (88, X'20067AB3686748B297FF46836CD4AA61', 'hair drier', 'black'),
           (89, X'FD13D02E02DE484DB813BA26EC19BF71', 'toothbrush', 'black');

create table recording_object_detection (
  camera_uuid not null check (length(camera_uuid) = 16),
  stream_name not null check (stream_name in ('main', 'sub')),
  recording_id integer not null,

  -- repeated:
  -- * frame delta unsigned varint
  -- * label unsigned varint
  -- * xmin, xmax, ymin, ymax as fixed 8-bit numbers
  --   (any value from knowing xmin <= xmax, ymin <= ymax?
  --   probably not a whole byte anyway.)
  --   although 256/300 or 256/320 is not super clean. awkward.
  -- * score/probability/whatever-it's-called as fixed 8-bit number
  --   linear scale?
  frame_data blob not null,

  -- Operations are almost always done on a bounded set of recordings, so
  -- and perhaps on all models. Use composite_id as the prefix of the primary
  -- key to make these efficient.
  primary key (camera_uuid, stream_name, recording_id)
);
